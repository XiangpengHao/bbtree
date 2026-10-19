#!/usr/bin/env bash
# Lays out a release build of the site for a static host: the post at
# post/, from its own copy of the page with the post's title, description
# and link-preview tags (the explorer's page names the explorer), and the
# explorer's page as the fallback for any other path.
#
#   vis/lay-out-site.sh vis/target/dx/bbtree-vis/release/web/public
set -euo pipefail

site=${1:?usage: lay-out-site.sh <dx public directory>}
title="bbtree: fastest ordered map"
description="How bbtree, a drop-in replacement for std's BTreeMap, cuts cache misses, instructions and cache coherence traffic, and how it is kept correct."
url="https://bbtree.xiangpeng.systems/post/"

mkdir -p "$site/post"
sed \
  -e "s|<title>[^<]*</title>|<title>$title</title>|" \
  -e "s|<meta name=\"description\" content=\"[^\"]*\" />|<meta name=\"description\" content=\"$description\" />\n    <meta property=\"og:type\" content=\"article\" />\n    <meta property=\"og:title\" content=\"$title\" />\n    <meta property=\"og:description\" content=\"$description\" />\n    <meta property=\"og:url\" content=\"$url\" />\n    <meta name=\"twitter:card\" content=\"summary\" />|" \
  "$site/index.html" > "$site/post/index.html"
grep -q "<title>$title</title>" "$site/post/index.html" \
  || { echo "lay-out-site.sh: no <title> to replace in $site/index.html" >&2; exit 1; }
grep -q 'og:title' "$site/post/index.html" \
  || { echo "lay-out-site.sh: no description meta to replace in $site/index.html" >&2; exit 1; }
cp "$site/index.html" "$site/404.html"
