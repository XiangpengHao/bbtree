#!/usr/bin/env bash
# Blog runs (blog-runs.sh, the architectures card's cells unless other
# arguments are given) on a throwaway EC2 instance: launches it (spot,
# Ubuntu 24.04), of the cheapest of the instance types and the region's
# zones that has room for it, builds
# the bench there for its CPU, copies the Wikipedia titles over, runs the
# cells, a key at a time when keys are named, copying the results to
# bench/results/blog/<name>-<date> after each, and on exit, however it
# exits, copies what's left, terminates the instance and deletes its key
# pair and security group. The instance also shuts itself down, and so
# terminates, two hours after it boots, should this script die first.
# Needs AWS credentials (aws login) and a default VPC in the region.
# ENTRIES and THREADS, if set, go to blog-runs.sh.
#
#   bench/tools/aws-blog-runs.sh <instance-types> <name> [<what> [keys...]]
#   bench/tools/aws-blog-runs.sh c9g.large aws-c9g
#   THREADS=1,8,16,24,32,40,48,56,64,72,80,88,96 bench/tools/aws-blog-runs.sh \
#       c8a.metal-24xl,c8a.24xlarge aws-c8a tab2 u64 uuid wiki-titles str-json
#
# The instance types, separated by commas, are of one architecture: the
# same machine, as a rule, bare metal or not. Bare metal boots slower.
#
# The architectures card's cells are one thread, so a small instance is
# enough for them: the build gets an 8 GB swap file and no debug info so
# that it fits in 4 GiB.
set -euo pipefail

usage="usage: $0 <instance-types> <name> [<what> [keys...]]"
types=${1:?$usage}
name=${2:?$usage}
shift 2
what=${1:-arch}
if [ $# -gt 0 ]; then shift; fi
keys=("$@")
repo=$(cd "$(dirname "$0")/../.." && pwd)
out=$repo/bench/results/blog/$name-$(date +%Y%m%d)
data=${BBTREE_BENCH_DATA:-${XDG_CACHE_HOME:-$HOME/.cache}/bbtree-bench}
tag=bbtree-blog-$name-$$
work=$(mktemp -d)
instance='' group='' host=''
ssh_options=(-i "$work/key" -o StrictHostKeyChecking=accept-new
    -o UserKnownHostsFile="$work/known_hosts" -o ConnectTimeout=10 -o BatchMode=yes)

cleanup() {
    if [ -n "$host" ]; then
        mkdir -p "$out"
        rsync -az -e "ssh ${ssh_options[*]}" "ubuntu@$host:results/" "$out/" || true
    fi
    if [ -n "$instance" ]; then
        aws ec2 terminate-instances --instance-ids "$instance" >/dev/null
        aws ec2 wait instance-terminated --instance-ids "$instance"
        echo "terminated $instance"
    fi
    # The group can go only once nothing uses it.
    [ -n "$group" ] && aws ec2 delete-security-group --group-id "$group" >/dev/null
    aws ec2 delete-key-pair --key-name "$tag" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT

architectures=$(aws ec2 describe-instance-types --instance-types ${types//,/ } \
    --query 'InstanceTypes[].ProcessorInfo.SupportedArchitectures[0]' --output text \
    | tr '\t' '\n' | sort -u)
case $architectures in
    x86_64) ubuntu=amd64 ;;
    arm64) ubuntu=arm64 ;;
    *) echo "$types: not one architecture this script knows" >&2; exit 2 ;;
esac
image=$(aws ssm get-parameter --query Parameter.Value --output text \
    --name "/aws/service/canonical/ubuntu/server/24.04/stable/current/$ubuntu/hvm/ebs-gp3/ami-id")

# Each type in each of the region's zones, the lowest spot price first.
mapfile -t offers < <(aws ec2 describe-spot-price-history --instance-types ${types//,/ } \
    --product-descriptions Linux/UNIX --start-time "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --query 'sort_by(SpotPriceHistory, &to_number(SpotPrice))[].[InstanceType,AvailabilityZone,SpotPrice]' \
    --output text)

aws ec2 create-key-pair --key-name "$tag" --query KeyMaterial --output text > "$work/key"
chmod 600 "$work/key"
group=$(aws ec2 create-security-group --group-name "$tag" \
    --description "bbtree blog run, deleted after" --query GroupId --output text)
aws ec2 authorize-security-group-ingress --group-id "$group" --protocol tcp --port 22 \
    --cidr "$(curl -fsS https://checkip.amazonaws.com)/32" >/dev/null
# The cheapest offer with spot capacity.
for offer in "${offers[@]}"; do
    read -r type zone price <<< "$offer"
    instance=$(aws ec2 run-instances --image-id "$image" --instance-type "$type" \
        --key-name "$tag" --security-group-ids "$group" --placement "AvailabilityZone=$zone" \
        --instance-market-options 'MarketType=spot,SpotOptions={SpotInstanceType=one-time,InstanceInterruptionBehavior=terminate}' \
        --instance-initiated-shutdown-behavior terminate \
        --block-device-mappings 'DeviceName=/dev/sda1,Ebs={VolumeSize=20,VolumeType=gp3,DeleteOnTermination=true}' \
        --user-data $'#!/bin/sh\nshutdown -h +120\n' \
        --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=$tag}]" \
        --query 'Instances[0].InstanceId' --output text 2>"$work/launch-error") && break
    grep -q InsufficientInstanceCapacity "$work/launch-error" || { cat "$work/launch-error" >&2; exit 1; }
    echo "no spot capacity for $type in $zone"
done
[ -n "$instance" ] || { echo "no spot capacity for $types in any zone" >&2; exit 1; }
echo "launched $instance ($type, $image) in $zone, \$$price an hour on spot"
aws ec2 wait instance-running --instance-ids "$instance"
host=$(aws ec2 describe-instances --instance-ids "$instance" \
    --query 'Reservations[0].Instances[0].PublicIpAddress' --output text)

# Bare metal can take 15 minutes to boot.
booting=$SECONDS
until ssh "${ssh_options[@]}" "ubuntu@$host" true 2>/dev/null; do
    [ $((SECONDS - booting)) -lt 1200 ] || { echo "no ssh to $host in 20 minutes" >&2; exit 1; }
    sleep 5
done
echo "ssh to $host after $((SECONDS - booting)) s"
rsync -az -e "ssh ${ssh_options[*]}" --exclude target --exclude .git --exclude vis \
    --exclude fuzz --exclude bench/results --exclude bench/wasm "$repo/" "ubuntu@$host:bbtree/"
if [ -d "$data/wiki-titles" ]; then
    rsync -az -e "ssh ${ssh_options[*]}" --mkpath "$data/wiki-titles/" \
        "ubuntu@$host:.cache/bbtree-bench/wiki-titles/"
fi

ssh "${ssh_options[@]}" "ubuntu@$host" bash -s <<'EOF'
set -euo pipefail
sudo fallocate -l 8G /swap && sudo chmod 600 /swap && sudo mkswap -q /swap && sudo swapon /swap
sudo DEBIAN_FRONTEND=noninteractive apt-get -qq update
sudo DEBIAN_FRONTEND=noninteractive apt-get -qq install -y build-essential >/dev/null
# The bench's toolchain (flake.nix's rustNightly).
curl -fsSL https://sh.rustup.rs | sh -s -- -y -q --profile minimal --default-toolchain nightly-2026-10-07
EOF

remote_env=(CARGO_PROFILE_RELEASE_DEBUG=false OUT=/home/ubuntu/results SINGLE_CPU=1)
for variable in ENTRIES THREADS; do
    if [ -n "${!variable:-}" ]; then remote_env+=("$variable=${!variable}"); fi
done
mkdir -p "$out"
# blog-runs.sh "$what" "$@" there, then every result copied back, so that
# an instance lost later loses only what it hadn't run.
run_and_copy() {
    ssh "${ssh_options[@]}" "ubuntu@$host" ". ~/.cargo/env && cd bbtree &&" \
        "$(printf '%q ' env "${remote_env[@]}" bash bench/tools/blog-runs.sh "$what" "$@")"
    rsync -az -e "ssh ${ssh_options[*]}" "ubuntu@$host:results/" "$out/"
}
if [ ${#keys[@]} -eq 0 ]; then
    run_and_copy
else
    for key in "${keys[@]}"; do run_and_copy "$key"; done
fi
echo "results in $out"
