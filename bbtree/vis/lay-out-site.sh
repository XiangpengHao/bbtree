#!/usr/bin/env bash
# Lays out a release build of the site for a static host: the post at
# post/, from its own copy of the page with the post's title, description
# and link-preview tags (the explorer's page names the explorer); the
# explorer at explorer/; the explorer's page as the fallback for any other
# path; and at the root, a page that sends the reader on without loading
# the site: to the post, or, for an explorer's link from when the explorer
# was the root (a query), to the explorer with it.
#
#   vis/lay-out-site.sh vis/target/dx/bbtree-vis/release/web/public
set -euo pipefail

site=${1:?usage: lay-out-site.sh <dx public directory>}
title="bbtree: fastest ordered map"
description="How bbtree, a drop-in replacement for std's BTreeMap, cuts cache misses, instructions and cache coherence traffic, and how it is kept correct."
url="https://bbtree.xiangpeng.systems/post/"
# The site's page, as dx built it; on a second run, the root holds the
# redirect, and the explorer's copy is the page.
page="$site/index.html"
if grep -q 'data-site-root' "$page"; then
  page="$site/explorer/index.html"
fi

mkdir -p "$site/post"
sed \
  -e "s|<title>[^<]*</title>|<title>$title</title>|" \
  -e "s|<meta name=\"description\" content=\"[^\"]*\" />|<meta name=\"description\" content=\"$description\" />\n    <meta property=\"og:type\" content=\"article\" />\n    <meta property=\"og:title\" content=\"$title\" />\n    <meta property=\"og:description\" content=\"$description\" />\n    <meta property=\"og:url\" content=\"$url\" />\n    <meta name=\"twitter:card\" content=\"summary\" />|" \
  "$page" > "$site/post/index.html"
grep -q "<title>$title</title>" "$site/post/index.html" \
  || { echo "lay-out-site.sh: no <title> to replace in $page" >&2; exit 1; }
grep -q 'og:title' "$site/post/index.html" \
  || { echo "lay-out-site.sh: no description meta to replace in $page" >&2; exit 1; }
mkdir -p "$site/explorer"
if [ "$page" != "$site/explorer/index.html" ]; then
  cp "$page" "$site/explorer/index.html"
fi
cp "$page" "$site/404.html"
cat > "$site/index.html" <<EOF
<!DOCTYPE html>
<html lang="en" data-site-root>
  <head>
    <meta charset="utf-8" />
    <title>$title</title>
    <link rel="canonical" href="$url" />
    <script>
      location.replace(location.search ? "explorer/" + location.search + location.hash : "post/" + location.hash);
    </script>
    <noscript><meta http-equiv="refresh" content="0; url=post/" /></noscript>
  </head>
  <body>
    <p><a href="post/">$title</a></p>
  </body>
</html>
EOF
