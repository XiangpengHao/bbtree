"""Checks the racing reads in compiled code: x86-64 assembly in Intel
syntax, or AArch64, as rustc writes it (`check.py` builds and runs this).

A version check is a load of a node's latch word, masked to drop the shared
holders' count (`NodeLatch::is_unchanged`), and a branch on its compare.
Once a check of a node passes, the code may read the node again only if
another check of the node follows on every path that returns. Otherwise what
it read reaches the caller unchecked: the reload a compiler may make of a
read it takes to be race-free. A read of the latch word itself, or of a node
the code holds latched, is no racing read. A path through a failed check, or
a jump to where one goes, restarts and throws away what it read.

Which register holds which node comes from a forward dataflow over each
function. A register copied, offset or untagged keeps its value's name; so
does a value stored to memory, while nothing writes there again. Any other
definition makes a new value, and so does a join where the paths into it
disagree. A walk along a path then follows the node it checked through the
values that hold it.
"""

import re
from collections import defaultdict

# `!SHARED_MASK`, as `is_unchanged` masks the latch word with it.
CHECK_MASKS = ("-2097151", "#0xffffffffffe00001")
# Where the latch word is, through a tagged (leaf) or an untagged pointer.
LATCH_OFFSETS = (7, 8)

X86_REGISTERS = {}
for full, names in {
    "rax": "rax eax ax al ah", "rbx": "rbx ebx bx bl bh", "rcx": "rcx ecx cx cl ch",
    "rdx": "rdx edx dx dl dh", "rsi": "rsi esi si sil", "rdi": "rdi edi di dil",
    "rbp": "rbp ebp bp bpl", "rsp": "rsp esp sp spl",
}.items():
    for name in names.split():
        X86_REGISTERS[name] = full
for n in range(8, 16):
    for suffix in ("", "d", "w", "b"):
        X86_REGISTERS[f"r{n}{suffix}"] = f"r{n}"
for n in range(32):
    for kind in ("xmm", "ymm", "zmm"):
        X86_REGISTERS[f"{kind}{n}"] = f"v{n}"
X86_64_BIT = {"rax", "rbx", "rcx", "rdx", "rsi", "rdi", "rbp", "rsp"} | {f"r{n}" for n in range(8, 16)}
X86_WIDTHS = {"byte": 1, "word": 2, "dword": 4, "qword": 8, "xmmword": 16, "ymmword": 32, "zmmword": 64}
X86_ARGUMENTS = ["rdi", "rsi", "rdx", "rcx", "r8", "r9"]
X86_CALLER_SAVED = ["rax", "rcx", "rdx", "rsi", "rdi", "r8", "r9", "r10", "r11"] + [f"v{n}" for n in range(32)]
X86_STORES = {
    "mov", "movq", "movd", "movdqa", "movdqu", "movups", "movaps", "vmovdqu", "vmovdqa", "vmovups",
    "vmovaps", "vmovq", "vmovd", "vmovdqu64", "vmovdqa64", "movbe", "movnti", "vpscatterqq",
    "vpextrq", "pextrq", "vextracti128", "setb", "sete", "setne", "seta", "setg", "sets",
    "vmovapd", "movsd", "movss",
}
ARM_ARGUMENTS = [f"x{n}" for n in range(8)]
ARM_CALLER_SAVED = [f"x{n}" for n in range(19)] + [f"v{n}" for n in range(32)]
ARM_WIDTHS = {"x": 8, "w": 4, "q": 16, "d": 8, "s": 4, "h": 2, "b": 1}


def split_operands(text):
    """Operands split at the commas outside brackets."""
    out, depth, current = [], 0, ""
    for ch in text:
        depth += (ch in "[{") - (ch in "]}")
        if ch == "," and depth == 0:
            out.append(current.strip())
            current = ""
        else:
            current += ch
    if current.strip():
        out.append(current.strip())
    return out


class Instruction:
    """One instruction, as the dataflow sees it.

    `defines` maps each register it writes to how: ("copy", register,
    offset or None), ("load", base, offset or None), or ("new",).
    `accesses` lists its memory accesses, (kind, base, offset or None),
    kind "load", "store" or "rmw"; `writes` what its stores and rmws write,
    (base, offset or None, width, the 64-bit register stored or None).
    """

    def __init__(self, line_no, text, arm):
        self.line_no, self.text = line_no, text.strip()
        self.op, rest = (self.text.split(None, 1) + [""])[:2]
        if not arm and self.op in ("lock", "rep"):
            prefixed, rest = (rest.split(None, 1) + [""])[:2]
            if self.op == "lock" or prefixed.startswith(("movs", "stos", "lods", "cmps", "scas")):
                self.op = f"{self.op} {prefixed}"
            else:
                self.op = prefixed  # rep bsf: tzcnt's encoding
        self.operands = split_operands(rest)
        self.defines, self.accesses, self.writes = {}, [], []
        self.call = self.ret = self.indirect = self.conditional = self.unconditional = False
        self.target = None  # a branch's label, or a call's symbol
        self.arguments = []
        # The address of the latch word it takes or lets go: (register, offset).
        self.latches = self.unlatches = None
        (self.decode_arm if arm else self.decode_x86)()

    @property
    def ends_path(self):
        return self.ret or (self.indirect and not self.call)

    # x86-64, Intel syntax

    @staticmethod
    def x86_register(operand):
        return X86_REGISTERS.get(operand.strip())

    @staticmethod
    def x86_address(operand):
        """(base register, offset or None if indexed), or None."""
        m = re.search(r"\[([^\]]+)\]", operand)
        if not m or "rip" in m.group(1):
            return None
        base, indexed, offset = None, False, 0
        for sign, term in re.findall(r"([+-]?)([^+-]+)", m.group(1).replace(" ", "")):
            if "*" in term or (term in X86_REGISTERS and base is not None):
                indexed = True
            elif term in X86_REGISTERS:
                base = X86_REGISTERS[term]
            else:
                try:
                    offset += int(term, 0) * (-1 if sign == "-" else 1)
                except ValueError:
                    return None
        return (base, None if indexed else offset) if base else None

    def decode_x86(self):
        op, ops = self.op, self.operands
        if op in ("ret", "ud2", "int3"):
            self.ret = True
            return
        if op == "call":
            self.call, self.arguments = True, X86_ARGUMENTS
            self.target = re.sub(r".*\[rip \+ |@\w+|\]", "", ops[0])
            self.indirect = self.x86_register(ops[0]) is not None
            for reg in X86_CALLER_SAVED:
                self.defines[reg] = ("new",)
            return
        if op == "jmp":
            self.unconditional = True
            if ops[0].startswith(".L"):
                self.target = ops[0]
            elif self.x86_register(ops[0]) or "[" in ops[0]:
                self.indirect = True
            else:
                self.ret = True  # a tail call
            return
        if op.startswith("j"):
            self.conditional, self.target = True, ops[0]
            return
        if op.startswith("rep "):
            if "movs" in op:
                self.accesses.append(("load", "rsi", None))
            self.accesses.append(("store", "rdi", None))
            self.writes.append(("rdi", None, None, None))
            for reg in ("rsi", "rdi", "rcx"):
                self.defines[reg] = ("new",)
            return
        if op.startswith("prefetch") or op in ("nop", "pause", "vzeroupper", "lfence", "mfence", "sfence", "push"):
            return
        if op == "pop":
            self.defines[self.x86_register(ops[0])] = ("new",)
            return
        if op == "lea":
            address = self.x86_address(ops[1])
            self.defines[self.x86_register(ops[0])] = ("copy", *address) if address else ("new",)
            return
        written_memory = bool(ops) and "[" in ops[0]
        for i, operand in enumerate(ops):
            address = self.x86_address(operand) if "[" in operand else None
            if not address:
                continue
            width = re.match(r"(\w+) ptr", operand.strip())
            width = X86_WIDTHS.get(width.group(1)) if width else None
            if i > 0 or (op.startswith(("cmp", "test", "bt")) and not op.startswith("lock")):
                kind = "load"
            elif op in X86_STORES:
                kind = "store"
            else:
                kind = "rmw"
            self.accesses.append((kind, *address))
            if kind != "load":
                stored = ops[1].strip() if kind == "store" and op == "mov" and width == 8 and len(ops) == 2 else None
                self.writes.append((*address, width, stored if stored in X86_64_BIT else None))
        if op.startswith("lock ") or op == "xchg":
            through = self.accesses[0][1:] if self.accesses else None
            if "cmpxchg" in op:
                self.latches = through
                self.defines["rax"] = ("new",)
            else:
                self.unlatches = through
                if len(ops) > 1 and self.x86_register(ops[1]):
                    self.defines[self.x86_register(ops[1])] = ("new",)
            return
        if not ops or op.startswith(("cmp", "test", "bt", "ucomi", "comi", "vptest", "ptest", "kortest", "ktest")):
            return
        dst = self.x86_register(ops[0])
        if dst is None:
            # A plain store to a latch word lets it go (`unlock_exclusive`).
            address = self.x86_address(ops[0])
            if written_memory and op in X86_STORES and address and ops[0].startswith("qword"):
                self.unlatches = address
            return
        whole = ops[0].strip() in X86_64_BIT
        src = ops[1].strip() if len(ops) == 2 else None
        if op == "mov" and whole and src in X86_64_BIT:
            self.defines[dst] = ("copy", src, 0)
        elif op == "mov" and whole and src.startswith("qword ptr"):
            address = self.x86_address(src)
            self.defines[dst] = ("load", *address) if address else ("new",)
        elif op in ("add", "sub") and whole and src and re.fullmatch(r"-?\d+", src):
            self.defines[dst] = ("copy", dst, int(src) * (1 if op == "add" else -1))
        elif op == "add" and whole and src in X86_64_BIT:
            self.defines[dst] = ("copy", dst, None)
        elif op == "and" and src in ("-2", "-8"):
            self.defines[dst] = ("copy", dst, 0)  # untagging
        elif op == "xchg" and src in X86_REGISTERS:
            self.defines[dst], self.defines[X86_REGISTERS[src]] = ("copy", X86_REGISTERS[src], 0), ("copy", dst, 0)
        else:
            self.defines[dst] = ("new",)

    # AArch64

    @staticmethod
    def arm_register(operand):
        operand = operand.strip()
        if m := re.fullmatch(r"[xw](\d+)", operand):
            return f"x{m.group(1)}"
        if m := re.fullmatch(r"[qdshbv](\d+)(\.\w+)?", operand):
            return f"v{m.group(1)}"
        return operand if operand in ("sp", "x29") else None

    @classmethod
    def arm_address(cls, operand):
        """(base register, offset or None if indexed), or None."""
        operand = operand.strip()
        if m := re.fullmatch(r"\[(\w+)(?:,\s*#(-?\w+))?\]!?", operand):
            return cls.arm_register(m.group(1)), int(m.group(2), 0) if m.group(2) else 0
        if m := re.fullmatch(r"\[(\w+),\s*\w+(?:,\s*\w+(?:\s*#\d+)?)?\]", operand):
            return cls.arm_register(m.group(1)), None
        return None

    def decode_arm(self):
        op, ops = self.op, self.operands
        if op in ("ret", "brk", "udf"):
            self.ret = True
            return
        if op in ("bl", "blr"):
            self.target = ops[0]
            if self.target.startswith("__aarch64_"):
                # Outline atomics: cas takes the address in x2, the others in
                # x1. They write that word and their scratch registers only.
                address = "x2" if "_cas" in self.target else "x1"
                if "_cas" in self.target and "_acq" in self.target:
                    self.latches = (address, 0)
                elif "_ldadd" in self.target and "_rel" in self.target:
                    self.unlatches = (address, 0)
                for reg in ("x0", "x16", "x17"):
                    self.defines[reg] = ("new",)
                self.writes.append((address, 0, 8, None))
                return
            self.call, self.arguments, self.indirect = True, ARM_ARGUMENTS, op == "blr"
            for reg in ARM_CALLER_SAVED:
                self.defines[reg] = ("new",)
            return
        if op in ("b", "br"):
            self.unconditional = True
            if op == "br":
                self.indirect = True
            elif ops[0].startswith(".L"):
                self.target = ops[0]
            else:
                self.ret = True  # a tail call
            return
        if op.startswith("b.") or op in ("cbz", "cbnz", "tbz", "tbnz"):
            self.conditional, self.target = True, ops[-1]
            return
        if op.startswith(("dmb", "dsb", "isb", "prfm", "nop", "hint", "yield")):
            return
        memory = [o for o in ops if o.strip().startswith("[")]
        if op.startswith(("ld", "st", "cas", "swp")) and memory:
            self.decode_arm_access(memory[0])
            return
        dst = self.arm_register(ops[0]) if ops else None
        if dst is None or op in ("cmp", "cmn", "tst", "ccmp", "fcmp"):
            return
        srcs = ops[1:]
        whole = ops[0].startswith("x")
        base = self.arm_register(srcs[0]) if srcs else None
        if op == "mov" and len(srcs) == 1 and whole and srcs[0].startswith(("x", "sp")) and base:
            self.defines[dst] = ("copy", base, 0)
        elif op in ("add", "sub") and len(srcs) == 2 and whole and base and srcs[1].startswith("#"):
            self.defines[dst] = ("copy", base, int(srcs[1][1:], 0) * (1 if op == "add" else -1))
        elif op == "add" and len(srcs) >= 2 and whole and base:
            self.defines[dst] = ("copy", base, None)
        elif op == "and" and len(srcs) == 2 and srcs[1] in ("#0xfffffffffffffffe", "#0xfffffffffffffff8"):
            self.defines[dst] = ("copy", base, 0)  # untagging
        else:
            self.defines[dst] = ("new",)

    def decode_arm_access(self, memory):
        op, ops = self.op, self.operands
        address = self.arm_address(memory)
        base, offset = address if address else (None, None)
        post_indexed = ops[-1] != memory
        registers = [o.strip() for o in ops if not o.strip().startswith(("[", "#"))]
        if op.startswith("ld") and not op.startswith("ldadd"):
            kind = "load"
        elif op.startswith(("cas", "swp", "ldadd")):
            kind = "rmw"
        else:
            kind = "store"
        if op.startswith(("stxr", "stlxr")):
            self.defines[self.arm_register(registers[0])] = ("new",)
            registers = registers[1:]
        if address and post_indexed:
            offset = 0  # at the base, before the step
        self.accesses.append((kind, base, offset))
        width = ARM_WIDTHS.get(registers[0][0], 8) if registers else 8
        if op.startswith(("str", "stur", "ldr", "ldur")) and op[-1] in "bh":
            width = 1 if op[-1] == "b" else 2
        if kind == "load":
            for n, operand in enumerate(registers):
                reg = self.arm_register(operand)
                known = operand.startswith("x") and base is not None and offset is not None
                if reg:
                    self.defines[reg] = ("load", base, offset + n * width) if known else ("new",)
        elif kind == "store":
            for n, operand in enumerate(registers):
                if base is not None:
                    at = None if offset is None else offset + n * width
                    self.writes.append((base, at, width, self.arm_register(operand) if operand.startswith("x") else None))
            if op == "stlr" and address:
                self.unlatches = (base, offset)
        else:
            if registers:
                self.defines[self.arm_register(registers[0])] = ("new",)
            if base is not None:
                self.writes.append((base, offset, width, None))
            if op.startswith("cas") and "a" in op[3:]:
                self.latches = (base, offset)
            elif op.startswith("ldadd") and "l" in op[5:]:
                self.unlatches = (base, offset)
        if address and base != "sp" and (memory.strip().endswith("!") or post_indexed):
            step = ops[-1].strip().lstrip("#") if post_indexed else str(address[1] or 0)
            self.defines[base] = ("copy", base, int(step, 0) if re.fullmatch(r"-?\w+", step) else None)


class Function:
    """One function's instructions, its values (dataflow) and its checks."""

    def __init__(self, name, lines, arm):
        self.name, self.arm = name, arm
        self.instructions, self.labels = [], {}
        for line_no, text in lines:
            if re.match(r"^\S.*:$", text):
                self.labels[text[:-1]] = len(self.instructions)
            elif text.strip():
                self.instructions.append(Instruction(line_no, text, arm))
        n = len(self.instructions)
        starts = {0} | set(self.labels.values())
        for i, ins in enumerate(self.instructions):
            if ins.conditional or ins.unconditional or ins.ret:
                starts.add(i + 1)
        self.block_start = {}
        starts = sorted(s for s in starts if s < n) + [n]
        for start, end in zip(starts, starts[1:]):
            for i in range(start, end):
                self.block_start[i] = start

    def successors(self, i):
        ins = self.instructions[i]
        if ins.ends_path:
            return []
        out = []
        if ins.target in self.labels and not ins.call:
            out.append(self.labels[ins.target])
        if not ins.unconditional and i + 1 < len(self.instructions):
            out.append(i + 1)
        return out

    # The dataflow: each register's and memory word's value, (name, offset),
    # before each instruction; memory words are keyed ("mem", the name of
    # their address's value, offset). And the values latched there.

    def find_values(self):
        n = len(self.instructions)
        entry = {reg: (f"arg:{reg}", 0) for reg in (ARM_ARGUMENTS if self.arm else X86_ARGUMENTS)}
        self.predecessors = predecessors = defaultdict(list)
        for i in range(n):
            for s in self.successors(i):
                predecessors[s].append(i)
        self.values_in, self.values_out = [None] * n, [None] * n
        self.latched_in, latched_out = [None] * n, [None] * n
        self.values_in[0], self.latched_in[0] = dict(entry), frozenset()
        # Code no branch reaches (a landing pad) starts knowing nothing.
        work = [0] + [i for i in range(1, n) if not predecessors[i]]
        queued = set(work)
        while work:
            i = work.pop()
            queued.discard(i)
            if i != 0 or predecessors[0]:
                incoming = [self.values_out[p] for p in predecessors[i] if self.values_out[p] is not None]
                if i == 0:
                    incoming.append(entry)
                if not incoming and predecessors[i]:
                    continue
                self.values_in[i] = self.join(i, incoming)
                self.latched_in[i] = self.join_latched(i, predecessors[i], latched_out)
            values, latched = self.transfer(i, self.values_in[i], self.latched_in[i])
            if values != self.values_out[i] or latched != latched_out[i]:
                self.values_out[i], latched_out[i] = values, latched
                for s in self.successors(i):
                    if s not in queued:
                        work.append(s)
                        queued.add(s)
        self.joined_at = defaultdict(dict)  # each join's new values, by register or word
        for i, values in enumerate(self.values_in):
            for key, value in (values or {}).items():
                if value[0] == f"phi:{i}:{key}":
                    self.joined_at[i][key] = value[0]
        self.held_at = [frozenset(v[0] for v in (values or {}).values()) for values in self.values_in]

    def join(self, i, incoming):
        joined = {}
        for key in set().union(*incoming):
            seen = {values.get(key) for values in incoming}
            joined[key] = seen.pop() if len(seen) == 1 and None not in seen else (f"phi:{i}:{key}", 0)
        # Memory known through a value the join makes new is an earlier
        # value's: another lane's state, say.
        for key in [k for k in joined if isinstance(k, tuple) and k[1].startswith(f"phi:{i}:")]:
            del joined[key]
        return joined

    def join_latched(self, i, predecessors, latched_out):
        """The values latched on every path in; a latched value stays latched
        under the name the join gives it."""
        paths = []
        for p in predecessors:
            if latched_out[p] is None:
                continue
            latched = set(latched_out[p])
            for key, value in self.values_out[p].items():
                if value[0] in latched_out[p] and key in self.values_in[i]:
                    latched.add(self.values_in[i][key][0])
            paths.append(frozenset(latched))
        if i == 0:
            paths.append(frozenset())
        return frozenset.intersection(*paths) if paths else frozenset()

    def transfer(self, i, values, latched):
        ins = self.instructions[i]
        out, latched = dict(values), set(latched)

        def value(reg):
            return values.get(reg, (f"entry:{reg}", 0))

        def word(base, offset):
            name, at = value(base)
            return name, (None if at is None or offset is None else at + offset)

        if ins.latches:
            latched.add(word(*ins.latches)[0])
        if ins.unlatches and word(*ins.unlatches)[1] in LATCH_OFFSETS:
            latched.discard(word(*ins.unlatches)[0])
        if self.contended_latch(ins):
            latched.add(value(ins.arguments[0])[0])
        # A store forgets what it may overlap, and remembers a register stored
        # whole. A call forgets memory but the stack's, and the stack where
        # an argument points.
        for base, offset, width, stored in ins.writes:
            name, at = word(base, offset)
            for key in [k for k in out if isinstance(k, tuple) and k[1] == name
                        and (at is None or at - 7 <= k[2] < at + (width or 8))]:
                del out[key]
            if stored and at is not None and width == 8:
                out[("mem", name, at)] = value(stored)
        if ins.call:
            stack = value("sp" if self.arm else "rsp")[0]
            pointed = [value(r)[1] for r in ins.arguments if value(r)[0] == stack and value(r)[1] is not None]
            for key in [k for k in out if isinstance(k, tuple)]:
                if key[1] != stack or any(p <= key[2] < p + 512 for p in pointed):
                    del out[key]
        for reg, how in ins.defines.items():
            if reg is None:
                continue
            if how[0] == "copy" and how[1] is not None:
                name, at = value(how[1])
                out[reg] = (name, None if how[2] is None or at is None else at + how[2])
            elif how[0] == "load" and word(how[1], how[2])[1] is not None and ("mem", *word(how[1], how[2])) in values:
                out[reg] = values[("mem", *word(how[1], how[2]))]
            else:
                out[reg] = (f"def:{i}:{reg}", 0)
                if how[0] == "load" and word(how[1], how[2])[1] is not None:
                    out[("mem", *word(how[1], how[2]))] = out[reg]
        # Memory known through a value this instruction makes new is an
        # earlier value's.
        new = {f"def:{i}:{reg}" for reg in ins.defines}
        for key in [k for k in out if isinstance(k, tuple) and k[1] in new]:
            del out[key]
        return out, frozenset(latched)

    @staticmethod
    def contended_latch(ins):
        """Whether ins calls the latch's wait for its word at the first
        argument (`NodeLatch::lock_shared`, `lock_exclusive`)."""
        return ins.call and re.search(r"lock_(shared|exclusive)_contended", ins.target or "")

    # The checks.

    def find_checks(self):
        """Each check: (index of the latch word's load, the node's value, index
        of the branch, where it goes if the check holds, where if it fails).
        A check branches on its compare (jne, je, b.ne, b.eq); or its compare
        is folded into a flag with other conditions, which the branch tests
        (`folded_fails`). What it can't make out as either goes to
        `unrecognized`, as does, on AArch64, an acquire barrier that no check
        follows, nor a check's load whose value it discards."""
        self.checks, self.unrecognized, folded = [], [], []
        for i, ins in enumerate(self.instructions):
            if ins.op != "and" or not any(mask in ins.text for mask in CHECK_MASKS):
                continue
            masked = ins.operands[1 if self.arm else 0]
            masked = ins.arm_register(masked) if self.arm else ins.x86_register(masked)
            loads = self.defining_loads(i, masked)
            if not loads:
                self.unrecognized.append(i)
                continue
            k = i + 1
            while k < len(self.instructions) and k not in self.labels.values():
                if self.instructions[k].conditional or self.instructions[k].unconditional or self.instructions[k].ret:
                    break
                k += 1
            branch = self.instructions[k] if k < len(self.instructions) else None
            for j, node in loads:
                if branch and branch.op in ("jne", "je", "b.ne", "b.eq"):
                    taken, falls = self.labels.get(branch.target), k + 1
                    holds, fails = (falls, taken) if branch.op in ("jne", "b.ne") else (taken, falls)
                    self.checks.append((j, node, k, holds, fails))
                else:
                    folded.append((i, j, node, k))
        restarts = {check[4] for check in self.checks}
        for i, j, node, k in folded:
            branch = self.instructions[k] if k < len(self.instructions) else None
            taken = self.labels.get(branch.target) if branch and branch.conditional else None
            fails = self.folded_fails(i, k)
            if taken is not None and (fails or (fails is None and taken in restarts)):
                self.checks.append((j, node, k, k + 1, taken))
            elif taken is not None and fails is False:
                self.checks.append((j, node, k, taken, k + 1))
            else:
                self.unrecognized.append(i)
        if self.arm:
            loads = {check[0] for check in self.checks}
            for i, ins in enumerate(self.instructions):
                follows = range(i + 1, min(i + 4, len(self.instructions)))
                if ins.text.split() == ["dmb", "ishld"] and not any(
                        j in loads or self.discards_latch_word(j) for j in follows):
                    self.unrecognized.append(i)
        self.check_through = {}
        for check in self.checks:
            _, base, offset = self.instructions[check[0]].accesses[0]
            self.check_through[check[0]] = (base, offset)
        self.check_branch = {check[2]: check for check in self.checks}
        self.restarts = {check[4] for check in self.checks if check[4] is not None}

    def defining_loads(self, i, reg):
        """The loads that define reg for instruction i, on every path into
        it, as (index, the value loaded through); empty if a path defines it
        otherwise, or further back than two blocks."""
        loads, seen = [], set()

        def back(j, start, depth):
            while j >= start:
                ins = self.instructions[j]
                if reg in ins.defines:
                    through = ins.accesses[0][1] if ins.accesses else None
                    if through is None or self.values_in[j] is None:
                        return False
                    loads.append((j, self.values_in[j].get(through, (f"entry:{through}", 0))[0]))
                    return True
                j -= 1
            if depth == 2 or not self.predecessors[start]:
                return False
            for p in self.predecessors[start]:
                if p not in seen:
                    seen.add(p)
                    if not back(p, self.block_start[p], depth + 1):
                        return False
            return True
        # Back from i within its block, then along every path into the block:
        # a compare that starts a block may join loads from several.
        return loads if back(i - 1, self.block_start[i], 0) else []

    def discards_latch_word(self, j):
        """Whether instruction j loads a latch word into the zero register: a
        check whose compare the compiler dropped, as every path goes on alike
        whatever the version. It vouches for nothing."""
        ins = self.instructions[j]
        if ins.op != "ldr" or ins.operands[0].strip() != "xzr" or not ins.accesses:
            return False
        _, base, offset = ins.accesses[0]
        at = (self.values_in[j] or {}).get(base, (None, 0))[1]
        return at is not None and offset is not None and at + offset in LATCH_OFFSETS

    def folded_fails(self, i, k):
        """For a check whose compare is folded into a flag (cset, setcc) that
        branch k tests: whether the branch taken means the check failed, or
        None for another shape. A flag set on ne is set on a mismatch, and
        joins other conditions by or; one set on eq, by and."""
        condition = None
        for x in range(i + 1, k):
            ins = self.instructions[x]
            if ins.op == "cset" and len(ins.operands) == 2:
                condition = ins.operands[1].strip()
            elif ins.op in ("setne", "sete"):
                condition = ins.op[3:]
            if condition:
                break
        op = self.instructions[k].op
        if condition not in ("ne", "eq") or op not in ("tbnz", "tbz", "cbnz", "cbz", "jne", "je"):
            return None
        return (op in ("tbnz", "cbnz", "jne")) == (condition == "ne")

    # Walks along paths, following a node by the values that hold it.

    def next_positions(self, i):
        """Where a path goes from i, but through no failed check or jump to
        where one goes: those restart."""
        if i in self.check_branch and self.check_branch[i][3] is not None:
            return [self.check_branch[i][3]]
        return [s for s in self.successors(i) if s not in self.restarts]

    def follow(self, p, s, path):
        """What a path knows of its node, (values, latched), as it goes from
        p to s. Its values are those that hold the node, as (name, offset):
        an address through the value at its own offset, plus the offset, is
        that far into the node. A value p makes, or one a join at s makes,
        is new: the one it replaces no longer holds the node (the next
        level's node, in a descent's loop). A join's value holds the node if
        the value the path brings to it does. The node is latched from p's
        taking its latch to p's letting it go."""
        node, latched = path
        ins, values = self.instructions[p], self.values_in[p]
        if ins.latches and self.into_node(node, values, *ins.latches) in LATCH_OFFSETS:
            latched = True
        if self.contended_latch(ins) and self.into_node(node, values, ins.arguments[0], 0) in LATCH_OFFSETS:
            latched = True
        if ins.unlatches and self.into_node(node, values, *ins.unlatches) in LATCH_OFFSETS:
            latched = False
        node = frozenset(a for a in node if not a[0].startswith(f"def:{p}:"))
        if joined := self.joined_at.get(s):
            brought = self.values_out[p]
            offsets = dict(node)
            kept = set()
            for key, name in joined.items():
                b_name, b_at = brought.get(key, (None, None))
                if b_name in offsets and b_at is not None:
                    kept.add((name, offsets[b_name] + b_at))
            node = frozenset(a for a in node if not a[0].startswith(f"phi:{s}:")) | kept
        held = self.held_at[s] if s < len(self.held_at) else frozenset()
        return frozenset(a for a in node if a[0] in held), latched

    def walk(self, starts, path, visit):
        """Visits every path from starts, (instruction, the one before), with
        what it knows of its node. visit(i, path) returns "stop" to end the
        path there, or something else than None to end the walk with."""
        seen = set()
        stack = [(s, self.follow(p, s, path)) for s, p in starts]
        while stack:
            i, path = stack.pop()
            while i is not None and i < len(self.instructions) and (i, path) not in seen:
                seen.add((i, path))
                verdict = visit(i, path)
                if verdict == "stop":
                    break
                if verdict is not None:
                    return verdict
                after = [] if self.instructions[i].ends_path else self.next_positions(i)
                if not after:
                    break
                stack.extend((n, self.follow(i, n, path)) for n in after[1:])
                i, path = after[0], self.follow(i, after[0], path)
        return None

    def into_node(self, node, values, base, offset):
        """How far into the node an access through base at offset is: None if
        base doesn't hold the node, "unknown" if the offset isn't known."""
        name, at = values.get(base, (None, None))
        offsets = dict(node)
        if name not in offsets:
            return None
        return "unknown" if at is None or offset is None else offsets[name] + at + offset

    def reads_node(self, i, path):
        """Whether instruction i reads the path's node, but its latch word,
        while the path doesn't hold it latched."""
        node, latched = path
        ins, values = self.instructions[i], self.values_in[i]
        if latched:
            return False
        for kind, base, offset in ins.accesses:
            if kind != "store" and base is not None:
                into = self.into_node(node, values, base, offset)
                if into is not None and into not in LATCH_OFFSETS:
                    return True
        if ins.call and re.search(r"memcpy|memmove|memcmp|bcmp", ins.target or ""):
            return any(self.into_node(node, values, reg, 0) is not None for reg in ins.arguments[:2])
        return False

    def checks_node(self, i, path):
        """Whether instruction i is the latch word's load of a check of the
        path's node."""
        if i not in self.check_through:
            return False
        base, offset = self.check_through[i]
        return self.into_node(path[0], self.values_in[i], base, offset) in LATCH_OFFSETS

    def unchecked_return(self, read, path):
        """A return that a path from the read reaches with no other check of
        the node on the way, or None."""
        def visit(i, path):
            if self.checks_node(i, path):
                return "stop"
            return i if self.instructions[i].ends_path else None
        return self.walk([(n, read) for n in self.next_positions(read)], path, visit)

    def audit(self):
        """The function's checks, and each (check, read, return) where a read
        of a node after one of its checks reaches a return unchecked."""
        self.find_values()
        self.find_checks()
        unchecked = []
        for load, node, branch, holds, _ in self.checks:
            # The node starts as the value the check loads its latch word
            # through, with the latch word where a tagged or an untagged
            # node's is.
            base, offset = self.check_through[load]
            at = self.values_in[load].get(base, (None, 0))[1]
            known = at is not None and offset is not None
            into = 0 if not known or at + offset in LATCH_OFFSETS else LATCH_OFFSETS[-1] - at - offset

            def visit(i, path, load=load):
                if not path[0] or (i != load and self.checks_node(i, path)):
                    return "stop"
                if self.reads_node(i, path):
                    end = self.unchecked_return(i, path)
                    if end is not None:
                        unchecked.append((load, i, end))
                return None
            latched = node in self.latched_in[load]
            self.walk([(holds, branch)], (frozenset([(node, into)]), latched), visit)
        return unchecked


def listing(path, roots):
    """The functions of the assembly file at path that roots reach by calls
    and jumps, without directives and comments; and whether it is AArch64."""
    lines = open(path).read().split("\n")
    arm = any(re.match(r"^\s+(ldr|stp|ldp|bl)\s", line) for line in lines)
    bodies, current = {}, None
    for line_no, line in enumerate(lines, 1):
        if m := re.match(r"^([^\s.#/][^:\s]*|\"[^\"]+\"):\s*(#.*)?$", line):
            current = m.group(1).strip('"')
            bodies[current] = []
        elif current is not None:
            if line.startswith(("\t.cfi_endproc", ".Lfunc_end")):
                current = None
            elif not re.match(r"^\s*(\.|#|//)", line) and line.strip():
                bodies[current].append((line_no, line))
            elif re.match(r"^\.L\w+:", line):
                bodies[current].append((line_no, line))
    reached, order = set(), []

    def reach(name):
        if name in reached or name not in bodies:
            return
        reached.add(name)
        order.append(name)
        for _, line in bodies[name]:
            if m := re.match(r"^\s+(call|jmp|bl|b|j[a-z]+|b\.[a-z]+|cbn?z|tbn?z)\s+(.*)$", line):
                for target in re.findall(r"[\w$.]+", m.group(2)):
                    reach(target)
    for root in roots:
        reach(root)
    return [Function(name, bodies[name], arm) for name in order], arm


def check(path, roots):
    """(checks, report lines) for the functions roots reach in path's
    assembly; the report lists each read a check doesn't vouch for and each
    check not made out."""
    functions, _ = listing(path, roots)
    checks, report = 0, []
    for f in functions:
        unchecked = f.audit()
        checks += len(f.checks)
        short = f.name if len(f.name) < 100 else f.name[:97] + "..."
        for i in f.unrecognized:
            report.append(f"{short}\n  not made out as a check, line {f.instructions[i].line_no}: {f.instructions[i].text}")
        for load, read, end in unchecked:
            report.append(
                f"{short}\n"
                f"  check, line {f.instructions[load].line_no}: {f.instructions[load].text}\n"
                f"  read,  line {f.instructions[read].line_no}: {f.instructions[read].text}\n"
                f"  return reached unchecked, line {f.instructions[end].line_no}: {f.instructions[end].text}")
    return checks, report
