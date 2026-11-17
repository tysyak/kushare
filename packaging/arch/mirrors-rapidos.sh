#!/usr/bin/env bash
# Reflector exprés para el contenedor de Arch: prueba en paralelo los
# primeros mirrors de una mirrorlist (por defecto la del host) bajando
# core.db con un límite de 4 s, y escribe en la salida estándar una
# mirrorlist con los 5 más rápidos. Si ninguno responde, la de respaldo.
#   mirrors-rapidos.sh [mirrorlist] [respaldo]
set -u
LIST=${1:-/etc/pacman.d/mirrorlist}
FALLBACK=${2:-https://geo.mirror.pkgbuild.com}
servers=$(sed -n 's#^Server *= *\(.*\)/\$repo/os/\$arch.*#\1#p' "$LIST" 2>/dev/null | head -20)
ranked=$(printf '%s\n' $servers | xargs -r -P 20 -I{} sh -c \
  'v=$(curl -s -o /dev/null -m 4 -w "%{speed_download} %{http_code}" "{}/core/os/x86_64/core.db"); \
   set -- $v; [ "${2:-0}" = 200 ] && echo "${1%.*} {}"' | sort -rn | head -5)
if [ -z "$ranked" ]; then
  echo "Server = $FALLBACK/\$repo/os/\$arch"
  echo "mirrors: ninguno respondió, se usa $FALLBACK" >&2
  exit 0
fi
printf '%s\n' "$ranked" | while read -r speed url; do
  echo "Server = $url/\$repo/os/\$arch"
  echo "mirror: $((speed / 1024)) KiB/s  $url" >&2
done
