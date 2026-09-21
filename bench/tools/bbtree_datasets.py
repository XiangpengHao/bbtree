"""Derives and characterizes bbtree's real-world string key datasets.

Every input is untrusted data: this script only parses it (CSV, TSV, XML, tar
listings, JSON, zip directories), never executes it. Run with `python3 -I`.

Each dataset's `SOURCE.md` says which subcommands derived it. `bbtree-bench
fetch` runs `cc-urls` itself, since Common Crawl's terms don't grant
redistributing the derived file.

Subcommands (`--help` after one describes its arguments):
  zip-fetch        extract zip members over HTTP range requests
  crate-names      crate names from crates.io's crates.csv
  tar-files        regular files of a tarball, top directory stripped
  json-keys        object keys and key vocabulary of JSON documents
  cc-urls          the distinct URLs of pages in random Common Crawl blocks
  ban-addresses    the distinct addresses of France's Base Adresse Nationale
  geonames-names   the distinct names of GeoNames' places
  dblp-titles      the distinct plain-text titles of dblp's publications
  stats            statistics of a file with one key per line
"""

import argparse
import csv
import gzip
import hashlib
import heapq
import http.client
import io
import json
import math
import os
import random
import re
import shutil
import sys
import tarfile
import time
import urllib.parse
import urllib.request
import xml.parsers.expat
import zipfile
from collections import Counter

USER_AGENT = "bbtree-bench (+https://github.com/XiangpengHao/bbtree)"


class HttpRangeFile(io.RawIOBase):
    """A read-only, seekable view of a remote file, read with HTTP range
    requests. Fails if the file changes (its ETag) between requests."""

    def __init__(self, url):
        request = urllib.request.Request(url, method="HEAD", headers={"User-Agent": USER_AGENT})
        with urllib.request.urlopen(request) as response:
            self.url = response.url  # after redirects
            self.size = int(response.headers["Content-Length"])
            self.etag = response.headers.get("ETag")
            self.last_modified = response.headers.get("Last-Modified")
        self.position = 0
        self.bytes_fetched = 0
        self.requests = 0

    def readable(self):
        return True

    def seekable(self):
        return True

    def tell(self):
        return self.position

    def seek(self, offset, whence=io.SEEK_SET):
        base = {io.SEEK_SET: 0, io.SEEK_CUR: self.position, io.SEEK_END: self.size}[whence]
        self.position = base + offset
        return self.position

    def readinto(self, buffer):
        end = min(self.position + len(buffer), self.size)
        if end <= self.position:
            return 0
        headers = {"User-Agent": USER_AGENT, "Range": f"bytes={self.position}-{end - 1}"}
        with urllib.request.urlopen(urllib.request.Request(self.url, headers=headers)) as response:
            if response.status != 206:
                raise RuntimeError(f"expected 206 Partial Content, got {response.status}")
            if response.headers.get("ETag") != self.etag:
                raise RuntimeError("the remote file changed during the fetch")
            data = response.read()
        if len(data) != end - self.position:
            raise RuntimeError("short range response")
        buffer[: len(data)] = data
        self.position = end
        self.bytes_fetched += len(data)
        self.requests += 1
        return len(data)


def zip_fetch(url, out_dir, members):
    remote = HttpRangeFile(url)
    print(f"url: {remote.url}\nsize: {remote.size}\netag: {remote.etag}\n"
          f"last-modified: {remote.last_modified}", file=sys.stderr)
    buffered = io.BufferedReader(remote, buffer_size=16 << 20)
    with zipfile.ZipFile(buffered) as archive:
        names = archive.namelist()
        for wanted in members:
            # Members sit under a dated top directory, e.g. 2026-10-07-020002/data/crates.csv.
            matches = [name for name in names if name == wanted or name.endswith("/" + wanted)]
            if len(matches) != 1:
                raise RuntimeError(f"{wanted}: expected one member, found {matches}")
            info = archive.getinfo(matches[0])
            print(f"member: {info.filename} compressed={info.compress_size} "
                  f"size={info.file_size} crc32={info.CRC:08x}", file=sys.stderr)
            target = os.path.join(out_dir, os.path.basename(wanted))
            with archive.open(info) as source, open(target, "wb") as sink:  # CRC-checked
                while chunk := source.read(1 << 20):
                    sink.write(chunk)
    print(f"fetched {remote.bytes_fetched} bytes in {remote.requests} range requests",
          file=sys.stderr)


def crate_names(crates_csv):
    csv.field_size_limit(1 << 30)
    with open(crates_csv, newline="", encoding="utf-8") as source:
        for row in csv.DictReader(source):
            name = row["name"]
            if "\n" in name or "\r" in name:
                raise RuntimeError(f"crate name with a newline: {name!r}")
            sys.stdout.write(name + "\n")


def tar_files(tarball):
    """Regular files only (no directories, symlinks or hard links), in archive
    order, with the top directory stripped. Never extracts."""
    dropped = Counter()
    out = sys.stdout.buffer
    with tarfile.open(tarball, mode="r|*") as archive:
        for member in archive:
            if not member.isreg():
                dropped["not a regular file"] += 1
                continue
            path = os.fsencode(member.name)
            top, separator, rest = path.partition(b"/")
            if not separator or not rest:
                raise RuntimeError(f"no top directory: {member.name!r}")
            if b"\n" in rest or b"\r" in rest:
                dropped["newline in path"] += 1
                continue
            rest.decode("utf-8")  # keys must be UTF-8
            out.write(rest + b"\n")
    print(f"dropped: {dict(dropped)}", file=sys.stderr)


class JsonObject:
    """One parsed JSON object: its (key, value) pairs in document order,
    duplicates kept."""

    def __init__(self, pairs):
        self.pairs = pairs

    def keys(self):
        return [key for key, _ in self.pairs]

    @staticmethod
    def preorder(value):
        """Every object in a parsed document, in order of its opening brace."""
        stack = [value]
        while stack:
            value = stack.pop()
            if isinstance(value, JsonObject):
                yield value
                stack.extend(reversed([child for _, child in value.pairs]))
            elif isinstance(value, list):
                stack.extend(reversed(value))


class KeyEscaping:
    """Keys go one per line or tab-separated: backslash, tab, newline and
    carriage return are escaped as \\\\, \\t, \\n and \\r."""

    ESCAPES = {"\\": "\\\\", "\t": "\\t", "\n": "\\n", "\r": "\\r"}

    @classmethod
    def escape(cls, key):
        return "".join(cls.ESCAPES.get(character, character) for character in key)

    @classmethod
    def needs_escape(cls, key):
        return any(character in cls.ESCAPES for character in key)


def padded_prefix(key, width):
    return key[:width].ljust(width, b"\0")


def json_keys(out_dir, named_docs):
    objects_by_doc = []
    for named in named_docs:
        name, _, path = named.partition("=")
        with open(path, encoding="utf-8") as source:
            document = json.load(source, object_pairs_hook=JsonObject)
        objects_by_doc.append((name, [obj.keys() for obj in JsonObject.preorder(document)]))

    vocabulary = Counter()
    escaped_keys = set()
    line = 0
    ranges = []
    with open(os.path.join(out_dir, "object-keys.tsv"), "w", encoding="utf-8", newline="\n") as sink:
        for name, objects in objects_by_doc:
            ranges.append((name, line + 1, line + len(objects)))
            for keys in objects:
                vocabulary.update(keys)
                escaped_keys.update(key for key in keys if KeyEscaping.needs_escape(key))
                sink.write("\t".join(KeyEscaping.escape(key) for key in keys) + "\n")
                line += 1
    with open(os.path.join(out_dir, "key-vocabulary.tsv"), "w", encoding="utf-8", newline="\n") as sink:
        for key, count in sorted(vocabulary.items(), key=lambda item: (-item[1], item[0].encode())):
            sink.write(f"{KeyEscaping.escape(key)}\t{count}\n")

    report = {"escaped keys": sorted(escaped_keys), "line ranges": ranges, "documents": {}}
    all_objects = []
    for name, objects in objects_by_doc:
        report["documents"][name] = JsonObjectStats(objects).summary()
        all_objects.extend(objects)
    report["all"] = JsonObjectStats(all_objects).summary()
    json.dump(report, sys.stdout, indent=1, ensure_ascii=False)
    print()


class RateLimit:
    """Holds a download's average rate at or under `bytes_per_second`, or
    doesn't limit it when that is None."""

    def __init__(self, bytes_per_second=None):
        self.bytes_per_second = bytes_per_second
        self.started = time.monotonic()
        self.transferred = 0

    def account(self, byte_count):
        """Counts `byte_count` more bytes, and sleeps while the average rate
        is over the limit."""
        self.transferred += byte_count
        if self.bytes_per_second:
            ahead = self.transferred / self.bytes_per_second - (time.monotonic() - self.started)
            if ahead > 0:
                time.sleep(ahead)


class SourceStream(io.RawIOBase):
    """A source file, local or downloaded over https, read once from start to
    end. Hashes what is read and holds a download under its rate limit."""

    def __init__(self, location, rate_limit):
        self.location = location
        self.rate_limit = rate_limit
        self.sha256 = hashlib.sha256()
        self.size = 0
        if location.startswith("https://"):
            request = urllib.request.Request(location, headers={"User-Agent": USER_AGENT})
            self.stream = urllib.request.urlopen(request, timeout=120)
            headers = self.stream.headers
            print(f"url: {self.stream.url}\nsize: {headers.get('Content-Length')}\n"
                  f"etag: {headers.get('ETag')}\nlast-modified: {headers.get('Last-Modified')}",
                  file=sys.stderr)
        else:
            self.stream = open(location, "rb")

    def readable(self):
        return True

    def readinto(self, buffer):
        count = self.stream.readinto(buffer)
        self.sha256.update(memoryview(buffer)[:count])
        self.size += count
        self.rate_limit.account(count)
        return count

    def close(self):
        self.stream.close()
        super().close()

    def gzip_text(self, newline):
        """The decompressed contents, as UTF-8 text; `newline` as `open`'s."""
        compressed = io.BufferedReader(self, buffer_size=1 << 20)
        return io.TextIOWrapper(gzip.GzipFile(fileobj=compressed), encoding="utf-8",
                                newline=newline)

    def report(self):
        """Prints what was read: call it after reading to the end."""
        print(f"source: {self.location}: {self.size} bytes, "
              f"SHA-256 {self.sha256.hexdigest()}", file=sys.stderr)


class DistinctKeys:
    """A dataset's distinct keys, written as a key file: one key per line, in
    byte order (as Rust's `String` orders them), in UTF-8.

    Keys the file can't hold exactly are skipped and counted: empty ones,
    those with a line break (LF, or CR, which Rust's `str::lines` would strip
    from a line's end), and those that aren't valid Unicode."""

    def __init__(self):
        self.keys = set()
        self.occurrences = 0
        self.tallies = Counter()

    def tally(self, observation):
        """Counts something about the source, for the report."""
        self.tallies[observation] += 1

    def add(self, key):
        if not key:
            return self.tally("skipped: empty key")
        if "\n" in key or "\r" in key:
            return self.tally("skipped: key with a line break")
        try:
            encoded = key.encode("utf-8")
        except UnicodeEncodeError:  # a lone surrogate, as from a JSON escape
            return self.tally("skipped: key that isn't valid Unicode")
        self.occurrences += 1
        self.keys.add(encoded)

    def write(self, path):
        """Writes the key file through a temporary one, so that a failed run
        leaves none behind, and prints what it wrote."""
        partial = path + ".partial"
        sha256 = hashlib.sha256()
        size = 0
        with open(partial, "wb") as sink:
            for key in sorted(self.keys):
                line = key + b"\n"
                sink.write(line)
                sha256.update(line)
                size += len(line)
        os.replace(partial, path)
        report = {"file": path, "bytes": size, "keys": len(self.keys), "sha256": sha256.hexdigest(),
                  "key occurrences": self.occurrences, "tallies": dict(sorted(self.tallies.items()))}
        json.dump(report, sys.stderr, indent=1, ensure_ascii=False)
        print(file=sys.stderr)


class CdxBlock:
    """A block of a Common Crawl CDX index: 3,000 index lines, compressed as
    one gzip member at `offset` in `cdx_file`. Blocks are numbered from 1, as
    `cluster.idx` numbers them."""

    def __init__(self, number, cdx_file, offset, length):
        self.number = number
        self.cdx_file = cdx_file
        self.offset = offset
        self.length = length

    @classmethod
    def from_cluster_idx(cls, entry):
        """The block that a line of `cluster.idx` lists: `<SURT key>
        <timestamp>`, then the CDX file, offset, length and block number,
        tab-separated."""
        _key, cdx_file, offset, length, number = entry.split("\t")
        return cls(int(number), cdx_file, int(offset), int(length))

    def rank(self, seed):
        """The block's place in a seeded random order of all blocks."""
        return hashlib.sha256(f"{seed}:{self.number}".encode()).digest()

    def cached_or_fetched(self, server, cache, rate_limit):
        """The block's gzip member, from `cache` if an earlier run stored it
        there, or else fetched from `server` (a `RangeClient` of the index's
        directory) and stored there."""
        path = os.path.join(cache, f"{self.number}.gz")
        if os.path.exists(path) and os.path.getsize(path) == self.length:
            with open(path, "rb") as source:
                return source.read()
        data = server.get_range(self.cdx_file, self.offset, self.length, rate_limit)
        if not data.startswith(b"\x1f\x8b"):
            raise RuntimeError(f"block {self.number} is not a gzip member")
        with open(path + ".partial", "wb") as sink:
            sink.write(data)
        os.replace(path + ".partial", path)
        return data


class RangeClient:
    """Byte ranges of the files in one directory of an https server, read one
    request at a time over a connection kept open between requests, which
    saves a TLS handshake per request. Transient failures (the server
    answers 503 when it is busy) are retried with exponential backoff."""

    def __init__(self, directory_url, attempts=6):
        parts = urllib.parse.urlsplit(directory_url)
        self.connection = http.client.HTTPSConnection(parts.netloc, timeout=60)
        self.directory = parts.path
        self.attempts = attempts

    def get_range(self, file_name, offset, length, rate_limit):
        path = f"{self.directory}/{file_name}"
        byte_range = f"bytes={offset}-{offset + length - 1}"
        for attempt in range(self.attempts):
            try:
                return self.get_once(path, byte_range, length, rate_limit)
            except (OSError, http.client.HTTPException) as error:
                self.connection.close()  # the next request opens a new one
                if attempt + 1 == self.attempts:
                    raise
                print(f"retrying {path} {byte_range} after: {error}", file=sys.stderr)
                time.sleep(5 * 2 ** attempt)

    def get_once(self, path, byte_range, length, rate_limit):
        self.connection.request("GET", path, headers={"User-Agent": USER_AGENT, "Range": byte_range})
        response = self.connection.getresponse()
        chunks = []
        while chunk := response.read(1 << 16):  # the whole body, to reuse the connection
            chunks.append(chunk)
            rate_limit.account(len(chunk))
        if response.status == 503:
            raise ConnectionError(f"{path} {byte_range}: 503 Service Unavailable")
        if response.status != 206:
            raise RuntimeError(f"{path} {byte_range}: expected 206 Partial Content, got {response.status}")
        data = b"".join(chunks)
        if len(data) != length:
            raise ConnectionError(f"{path} {byte_range}: {len(data)} bytes")
        return data


class CommonCrawlUrls:
    """The URLs of the pages in random blocks of a Common Crawl crawl's CDX
    index, which is sorted by SURT key (the URL with its host reversed) and
    cut into blocks that `cluster.idx` lists.

    - **Blocks:** the `count` blocks whose SHA-256 of `<seed>:<block number>`
      is smallest. Unlike `random.sample`'s, this choice can't change with
      the Python version, so the derived file can be pinned exactly. The
      blocks spread over the whole key space; each is a run of neighbouring
      URLs, mostly of one site.
    - **Captures:** those in the crawl's `warc` subset, the pages it archived.
      Captures in `crawldiagnostics` (redirects, 404s and other errors) and
      `robotstxt` are skipped.
    - **URLs:** as crawled (the record's `url`), not as SURT keys.

    The blocks are fetched one at a time and kept in `<out>.blocks/` until
    the key file is written, so that a run that stops (the server answers
    503 when it is busy) resumes where it stopped."""

    # A CDX line: `<SURT key> <14-digit timestamp> <JSON object>`.
    CDX_LINE = re.compile(r" \d{14} (\{.*\})$")

    def __init__(self, count, seed):
        self.count = count
        self.seed = seed
        self.keys = DistinctKeys()

    def derive(self, cluster_idx, index_url, out, rate_limit):
        with SourceStream(cluster_idx, rate_limit) as source:
            entries = source.read().decode("utf-8").splitlines()
            source.report()
        blocks = [CdxBlock.from_cluster_idx(entry) for entry in entries]
        chosen = sorted(heapq.nsmallest(self.count, blocks, key=lambda block: block.rank(self.seed)),
                        key=lambda block: block.number)
        print(f"chose {len(chosen)} of {len(blocks)} blocks", file=sys.stderr)
        cache = out + ".blocks"
        os.makedirs(cache, exist_ok=True)
        server = RangeClient(index_url)
        fetched = hashlib.sha256()
        fetched_bytes = 0
        for index, block in enumerate(chosen):
            data = block.cached_or_fetched(server, cache, rate_limit)
            fetched.update(data)
            fetched_bytes += len(data)
            self.add_captures(gzip.decompress(data))
            if (index + 1) % 500 == 0 or index + 1 == len(chosen):
                print(f"{index + 1}/{len(chosen)} blocks, {fetched_bytes} bytes", file=sys.stderr)
        print(f"fetched blocks: {fetched_bytes} bytes, SHA-256 of their concatenation "
              f"{fetched.hexdigest()}", file=sys.stderr)
        self.keys.write(out)
        shutil.rmtree(cache)

    def add_captures(self, block_lines):
        # Undecodable bytes become lone surrogates, which `DistinctKeys` skips.
        for line in block_lines.decode("utf-8", errors="surrogateescape").splitlines():
            match = self.CDX_LINE.search(line)
            if not match:
                raise RuntimeError(f"not a CDX line: {line!r}")
            capture = json.loads(match.group(1))
            # crawl-data/<crawl>/segments/<segment>/<subset>/<file>
            subset = capture["filename"].split("/")[4]
            self.keys.tally(f"captures in {subset}")
            if subset == "warc":
                self.keys.add(capture["url"])


def ban_addresses(location, out):
    """The distinct addresses of the Base Adresse Nationale's
    `adresses-france.csv.gz`, each in the form of the `label` that the BAN's
    geocoder (api-adresse.data.gouv.fr) returns:
    `{numero}{rep} {nom_voie} {code_postal} {nom_commune}`, such as
    `83bis Chemin du Peuple 01330 Ambérieux-en-Dombes`. An empty part is left
    out with its space."""
    keys = DistinctKeys()
    parts = ("numero", "rep", "nom_voie", "code_postal", "nom_commune")
    with SourceStream(location, RateLimit()) as source:
        rows = csv.reader(source.gzip_text(newline=""), delimiter=";")
        header = next(rows)
        columns = [header.index(part) for part in parts]
        for row in rows:
            if len(row) != len(header):
                raise RuntimeError(f"a row of {len(row)} fields, not {len(header)}: {row}")
            numero, rep, street, postcode, commune = (row[column] for column in columns)
            for part, value in (("numero", numero), ("nom_voie", street),
                                ("code_postal", postcode), ("nom_commune", commune)):
                if not value:
                    keys.tally(f"rows without {part}")
            if numero == "99999":
                keys.tally("rows with numero 99999")
            keys.add(" ".join(value for value in (numero + rep, street, postcode, commune) if value))
        source.report()
    keys.write(out)


def geonames_names(zip_path, out):
    """The distinct `name` values of GeoNames' `allCountries.zip`: each place's
    name in UTF-8 (not its `asciiname`), kept exactly. Its `allCountries.txt`
    is tab-separated, without quoting or a header, 19 fields a line."""
    keys = DistinctKeys()
    with zipfile.ZipFile(zip_path) as archive, archive.open("allCountries.txt") as member:
        for line in io.TextIOWrapper(member, encoding="utf-8", newline="\n"):  # CRC-checked
            fields = line.removesuffix("\n").split("\t")
            if len(fields) != 19:
                raise RuntimeError(f"a line of {len(fields)} fields, not 19: {line!r}")
            keys.add(fields[1])
    keys.write(out)


class DblpTitleParser:
    """The titles of dblp's publication records, as plain text, parsed from
    `dblp.xml.gz` as it streams by.

    A title's plain text is all the character data inside its `<title>`:
    markup elements (`<i>`, `<sub>`, `<sup>`, `<tt>`, `<ref>`) are dropped
    and their text kept, so `H<sub>2</sub>O` becomes `H2O`. Entities become
    their characters as `dblp.dtd` defines them (`&auml;` is `ä`), and
    character references theirs (`&#8211;` is `–`). Nothing else changes:
    whitespace and trailing periods are kept."""

    # Every record type but `www` (home pages, whose title is "Home Page"),
    # `person` and `data` (research data).
    PUBLICATIONS = frozenset(["article", "inproceedings", "proceedings", "book", "incollection",
                              "phdthesis", "mastersthesis"])

    def __init__(self, dtd_name, dtd):
        self.dtd_name = dtd_name
        self.dtd = dtd
        self.depth = 0
        self.record = None
        self.title_parts = None  # while inside a publication's <title>
        self.records = Counter()
        self.keys = DistinctKeys()
        self.parser = xml.parsers.expat.ParserCreate()
        self.parser.buffer_text = True
        self.parser.buffer_size = 1 << 20
        self.parser.SetParamEntityParsing(xml.parsers.expat.XML_PARAM_ENTITY_PARSING_ALWAYS)
        self.parser.ExternalEntityRefHandler = self.read_dtd
        self.parser.StartElementHandler = self.start_element
        self.parser.EndElementHandler = self.end_element
        self.parser.CharacterDataHandler = self.character_data

    def parse(self, stream):
        self.parser.ParseFile(stream)

    def read_dtd(self, context, _base, system_id, _public_id):
        """Reads the document's external DTD from the local copy, which is
        pinned and must have the name that the document gives, such as
        `dblp-2023-06-28.dtd`."""
        if system_id != self.dtd_name:
            raise RuntimeError(f"the document's DTD is {system_id!r}, not {self.dtd_name!r}")
        self.parser.ExternalEntityParserCreate(context).Parse(self.dtd, True)
        return 1

    def start_element(self, name, _attributes):
        self.depth += 1
        if self.depth == 2:  # <dblp> holds the records
            self.record = name
            self.records[name] += 1
        elif self.depth == 3 and name == "title" and self.record in self.PUBLICATIONS:
            self.title_parts = []

    def end_element(self, _name):
        if self.depth == 3 and self.title_parts is not None:
            self.keys.add("".join(self.title_parts))
            self.title_parts = None
        self.depth -= 1

    def character_data(self, data):
        if self.title_parts is not None:
            self.title_parts.append(data)


def dblp_titles(xml_gz, dtd_path, out):
    with open(dtd_path, "rb") as source:
        dtd = source.read()
    titles = DblpTitleParser(os.path.basename(dtd_path), dtd)
    with SourceStream(xml_gz, RateLimit()) as source:
        titles.parse(gzip.GzipFile(fileobj=io.BufferedReader(source, buffer_size=1 << 20)))
        source.report()
    print(f"records: {dict(titles.records.most_common())}", file=sys.stderr)
    titles.keys.write(out)


class Distribution:
    """Order statistics of a list of numbers; percentiles by nearest rank."""

    def __init__(self, values):
        self.values = sorted(values)

    def percentile(self, percent):
        rank = max(1, math.ceil(percent / 100 * len(self.values)))
        return self.values[rank - 1]

    def summary(self):
        if not self.values:
            return {}
        return {
            "min": self.values[0],
            "median": self.percentile(50),
            "mean": round(sum(self.values) / len(self.values), 2),
            "p90": self.percentile(90),
            "p99": self.percentile(99),
            "max": self.values[-1],
        }


class JsonObjectStats:
    """Per-object statistics: each object becomes one small map."""

    def __init__(self, objects):
        self.objects = objects

    def summary(self):
        occurrences = [key.encode() for keys in self.objects for key in keys]
        tied_objects = 0
        tied_pairs = 0
        neighbour_pairs = 0
        duplicate_objects = 0
        for keys in self.objects:
            distinct = sorted({key.encode() for key in keys})
            if len(distinct) != len(keys):
                duplicate_objects += 1
            prefixes = [padded_prefix(key, 8) for key in distinct]
            if len(set(prefixes)) != len(prefixes):
                tied_objects += 1
            for left, right in zip(prefixes, prefixes[1:]):
                neighbour_pairs += 1
                tied_pairs += left == right
        multi_key = sum(1 for keys in self.objects if len(keys) >= 2)
        return {
            "objects": len(self.objects),
            "empty objects": sum(1 for keys in self.objects if not keys),
            "objects with duplicate keys": duplicate_objects,
            "keys per object": Distribution(len(keys) for keys in self.objects).summary(),
            "objects with <= 8 keys": round(
                sum(1 for keys in self.objects if len(keys) <= 8) / len(self.objects), 4),
            "key occurrences": KeySetStats(occurrences).summary(),
            "within-object 8-byte tie, fraction of objects": round(tied_objects / len(self.objects), 4),
            "within-object 8-byte tie, fraction of objects with >= 2 keys":
                round(tied_objects / multi_key, 4) if multi_key else None,
            "within-object 8-byte tie, fraction of sorted neighbour pairs":
                round(tied_pairs / neighbour_pairs, 4) if neighbour_pairs else None,
        }


class KeySetStats:
    """Length statistics over all keys; prefix statistics over the sorted
    distinct keys (byte order, as `String`'s `Ord`)."""

    def __init__(self, keys):
        self.keys = keys

    @staticmethod
    def common_prefix_length(left, right):
        width = min(len(left), len(right))
        difference = int.from_bytes(left[:width]) ^ int.from_bytes(right[:width])
        return (8 * width - difference.bit_length()) // 8

    @staticmethod
    def tie_rate(sorted_keys, width):
        """The share of sorted neighbours whose zero-padded `width`-byte
        prefixes are equal."""
        pairs = len(sorted_keys) - 1
        if pairs <= 0:
            return None
        ties = sum(padded_prefix(left, width) == padded_prefix(right, width)
                   for left, right in zip(sorted_keys, sorted_keys[1:]))
        return round(ties / pairs, 4)

    def summary(self, top_groups=0, sample_sizes=(), sample_seed=42):
        """Also gives the tie rates of random samples of the distinct keys,
        each drawn with `random.Random(sample_seed).sample`, then sorted."""
        lengths = Distribution(len(key) for key in self.keys)
        distinct = sorted(set(self.keys))
        pairs = len(distinct) - 1
        lcp_total = 0
        for left, right in zip(distinct, distinct[1:]):
            lcp_total += self.common_prefix_length(left, right)
        prefix8_groups = Counter(padded_prefix(key, 8) for key in distinct)
        summary = {
            "count": len(self.keys),
            "distinct": len(distinct),
            "bytes": lengths.summary(),
            "share <= 8 B": round(sum(1 for key in self.keys if len(key) <= 8) / len(self.keys), 4),
            "8-byte tie rate": self.tie_rate(distinct, 8),
            "16-byte tie rate": self.tie_rate(distinct, 16),
            "mean neighbour LCP": round(lcp_total / pairs, 2) if pairs > 0 else None,
            "distinct 8-byte prefixes": len(prefix8_groups),
        }
        if top_groups:
            summary["largest 8-byte prefix groups"] = [
                [prefix.rstrip(b"\0").decode("utf-8", "backslashreplace"), count]
                for prefix, count in prefix8_groups.most_common(top_groups)
            ]
        for size in sample_sizes:
            sample = sorted(random.Random(sample_seed).sample(distinct, size))
            summary[f"sample of {size}"] = {
                "8-byte tie rate": self.tie_rate(sample, 8),
                "16-byte tie rate": self.tie_rate(sample, 16),
            }
        return summary


def stats(key_file, sample_sizes):
    with open(key_file, "rb") as source:
        keys = source.read().split(b"\n")
    if keys and keys[-1] == b"":
        keys.pop()
    summary = KeySetStats(keys).summary(top_groups=15, sample_sizes=sample_sizes)
    json.dump(summary, sys.stdout, indent=1, ensure_ascii=False)
    print()


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True, metavar="subcommand")
    max_rate = {"type": int, "metavar": "BYTES_PER_SECOND",
                "help": "hold the download's average rate at or under this"}

    command = commands.add_parser("zip-fetch", help="extract zip members over HTTP range requests")
    command.add_argument("url")
    command.add_argument("out_dir")
    command.add_argument("members", nargs="+")
    command.set_defaults(run=lambda a: zip_fetch(a.url, a.out_dir, a.members))

    command = commands.add_parser("crate-names", help="crate names from crates.csv, to stdout")
    command.add_argument("crates_csv")
    command.set_defaults(run=lambda a: crate_names(a.crates_csv))

    command = commands.add_parser("tar-files", help="regular files of a tarball, to stdout")
    command.add_argument("tarball")
    command.set_defaults(run=lambda a: tar_files(a.tarball))

    command = commands.add_parser("json-keys", help="object keys of JSON documents")
    command.add_argument("out_dir")
    command.add_argument("named_documents", nargs="+", metavar="NAME=DOCUMENT")
    command.set_defaults(run=lambda a: json_keys(a.out_dir, a.named_documents))

    command = commands.add_parser("cc-urls", help="distinct URLs of pages in random index blocks")
    command.add_argument("cluster_idx", help="the index's block listing: a path, or an https URL")
    command.add_argument("index_url", help="the directory of the index's cdx-NNNNN.gz files")
    command.add_argument("count", type=int, help="how many blocks to fetch")
    command.add_argument("seed")
    command.add_argument("out")
    command.add_argument("--max-rate", **max_rate)
    command.set_defaults(run=lambda a: CommonCrawlUrls(a.count, a.seed).derive(
        a.cluster_idx, a.index_url, a.out, RateLimit(a.max_rate)))

    command = commands.add_parser("ban-addresses", help="distinct addresses of BAN's CSV export")
    command.add_argument("adresses_csv_gz")
    command.add_argument("out")
    command.set_defaults(run=lambda a: ban_addresses(a.adresses_csv_gz, a.out))

    command = commands.add_parser("geonames-names", help="distinct names of GeoNames' places")
    command.add_argument("all_countries_zip")
    command.add_argument("out")
    command.set_defaults(run=lambda a: geonames_names(a.all_countries_zip, a.out))

    command = commands.add_parser("dblp-titles", help="distinct plain-text titles of dblp")
    command.add_argument("dblp_xml_gz")
    command.add_argument("dblp_dtd")
    command.add_argument("out")
    command.set_defaults(run=lambda a: dblp_titles(a.dblp_xml_gz, a.dblp_dtd, a.out))

    command = commands.add_parser("stats", help="statistics of a key file, to stdout")
    command.add_argument("key_file")
    command.add_argument("--sample-sizes", default="", metavar="N,N,...",
                         help="also the tie rates of random samples of these sizes")
    command.set_defaults(run=lambda a: stats(
        a.key_file, [int(size) for size in a.sample_sizes.split(",") if size]))

    arguments = parser.parse_args()
    arguments.run(arguments)


if __name__ == "__main__":
    main()
