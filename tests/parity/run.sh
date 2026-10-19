#!/bin/sh
# Runs komposer and Composer side by side and compares what they leave behind (vendor/, composer.json,
# composer.lock) and, for the scripts in scripts/, what they print and their exit codes.
#
# Needs php, Composer 2.10.3's composer.phar ($COMPOSER_PHAR) and a komposer build ($KOMPOSER, default
# target/release/komposer). $WORK is the scratch directory (default: a new temporary one).
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
W=${WORK:-$(mktemp -d)}
K=${KOMPOSER:-$ROOT/target/release/komposer}
PHAR=${COMPOSER_PHAR:?set COMPOSER_PHAR to Composer 2.10.3\'s composer.phar}
mkdir -p "$W"
failed=0

check() { if eval "$2"; then echo "PASS $1"; else echo "FAIL $1"; failed=1; return 1; fi; }
oc() { dir=$1; shift; (cd "$dir" && COMPOSER_HOME=$W/home-composer COMPOSER_CACHE_DIR=$W/cache-composer php "$PHAR" "$@"); }
kc() { dir=$1; shift; (cd "$dir" && COMPOSER_HOME=$W/home-komposer COMPOSER_CACHE_DIR=$W/cache-komposer "$K" "$@"); }
fresh() { rm -rf "$1" && mkdir -p "$1" && cp "$ROOT/tests/fixtures/laravel-13/composer.json" "$ROOT/tests/fixtures/laravel-13/composer.lock" "$1/"; }

# a Laravel app: the files both tools write
fresh "$W/i-o"; fresh "$W/i-k"
oc "$W/i-o" install --no-scripts --no-progress > "$W/i-o.log" 2>&1 & kc "$W/i-k" install --no-scripts > "$W/i-k.log" 2>&1; wait
check install "diff -rq '$W/i-k/vendor' '$W/i-o/vendor' >/dev/null"

for d in u-o u-k; do rm -rf "$W/$d"; mkdir -p "$W/$d"; cp "$ROOT/tests/fixtures/laravel-13/composer.json" "$W/$d/"; done
oc "$W/u-o" update --no-scripts --no-progress --no-install > "$W/u-o.log" 2>&1 & kc "$W/u-k" update --no-scripts --no-install > "$W/u-k.log" 2>&1; wait
check update "cmp -s '$W/u-k/composer.lock' '$W/u-o/composer.lock'"

rm -rf "$W/r-o" "$W/r-k"; cp -R "$W/i-o" "$W/r-o"; cp -R "$W/i-o" "$W/r-k"
oc "$W/r-o" require laravel/sanctum --no-scripts --no-progress > "$W/r-o.log" 2>&1 & kc "$W/r-k" require laravel/sanctum --no-scripts > "$W/r-k.log" 2>&1; wait
check require "cmp -s '$W/r-k/composer.json' '$W/r-o/composer.json' && cmp -s '$W/r-k/composer.lock' '$W/r-o/composer.lock' && diff -rq '$W/r-k/vendor' '$W/r-o/vendor' >/dev/null"

rm -rf "$W/rm-o" "$W/rm-k"; cp -R "$W/r-o" "$W/rm-o"; cp -R "$W/r-o" "$W/rm-k"
oc "$W/rm-o" remove laravel/sanctum laravel/tinker --no-scripts --no-progress > "$W/rm-o.log" 2>&1 & kc "$W/rm-k" remove laravel/sanctum laravel/tinker --no-scripts > "$W/rm-k.log" 2>&1; wait
check remove "cmp -s '$W/rm-k/composer.json' '$W/rm-o/composer.json' && cmp -s '$W/rm-k/composer.lock' '$W/rm-o/composer.lock' && diff -rq '$W/rm-k/vendor' '$W/rm-o/vendor' >/dev/null"

rm -rf "$W/cp-o" "$W/cp-k"; mkdir -p "$W/cp-o" "$W/cp-k"
oc "$W/cp-o" create-project laravel/laravel app --no-scripts --no-progress > "$W/cp-o.log" 2>&1 & kc "$W/cp-k" create-project laravel/laravel app --no-scripts > "$W/cp-k.log" 2>&1; wait
check create-project "diff -rq '$W/cp-k/app' '$W/cp-o/app' >/dev/null"

rm -rf "$W/da-k"; cp -R "$W/i-o" "$W/da-k"; rm -f "$W/da-k/vendor/autoload.php" "$W/da-k"/vendor/composer/autoload_*.php
kc "$W/da-k" dump-autoload -o --no-scripts > "$W/da-k.log" 2>&1
check dump-autoload "diff -rq '$W/da-k/vendor' '$W/i-o/vendor' >/dev/null"

# scripts: output and exit codes, stdout and stderr apart (how they interleave varies between runs)
rm -rf "$W/s-o" "$W/s-k" "$W/out"; mkdir -p "$W/out"
cp -R "$HERE/scripts" "$W/s-o"; cp -R "$HERE/scripts" "$W/s-k"
n=0
while IFS= read -r line; do
  [ -z "$line" ] && continue
  n=$((n+1))
  eval "set -- $line"
  oc "$W/s-o" "$@" --no-ansi < /dev/null > "$W/out/$n.o" 2> "$W/out/$n.o.err"; echo "exit $?" >> "$W/out/$n.o"
  kc "$W/s-k" "$@" --no-ansi < /dev/null > "$W/out/$n.k" 2> "$W/out/$n.k.err"; echo "exit $?" >> "$W/out/$n.k"
  if ! check "script: $line" "cmp -s '$W/out/$n.k' '$W/out/$n.o' && cmp -s '$W/out/$n.k.err' '$W/out/$n.o.err'"; then
    diff "$W/out/$n.o" "$W/out/$n.k"; diff "$W/out/$n.o.err" "$W/out/$n.k.err"
  fi
done < "$HERE/scripts/commands.txt"

exit $failed
