#!/bin/sh
# Dependency policy gate (architecture D2/D3, specification §1): no async
# runtimes, no serde stack, no C TLS. Exact-name match, so `serde` ≠ `miniserde`.
set -eu
deps=$(cargo tree --prefix none | awk '{print $1}' | sort -u)
status=0
for bad in tokio async-std smol futures hyper reqwest serde serde_json serde_derive \
    openssl openssl-sys native-tls; do
  if printf '%s\n' "$deps" | grep -qx "$bad"; then
    echo "forbidden dependency present: $bad" >&2
    status=1
  fi
done
[ "$status" -eq 0 ] && echo "dependency policy: clean"
exit $status
