#!/bin/bash
# Проверить, не ушёл ли апстрим tester-bcs/luck (директория rust/) вперёд
# относительно пина в luck-pilot/vendor/VENDOR.md.
set -euo pipefail

REPO_URL="https://github.com/tester-bcs/luck.git"
PIN_FILE="$(dirname "$0")/../luck-pilot/vendor/VENDOR.md"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

PINNED=$(grep -oP '(?<=\| Коммит \(последний, трогающий `rust/`\) \| `)[0-9a-f]+' "$PIN_FILE" || true)
if [ -z "$PINNED" ]; then
  echo "Не удалось прочитать текущий пин из $PIN_FILE" >&2
  exit 2
fi

echo "== Пин в vendor/VENDOR.md: $PINNED =="
echo "== Клонирую апстрим ($REPO_URL)... =="
git clone --quiet "$REPO_URL" "$TMPDIR/luck"

cd "$TMPDIR/luck"
LATEST=$(git log -1 --format="%H" -- rust/)

if [ "$LATEST" = "$PINNED" ]; then
  echo "== АКТУАЛЬНО: vendor/luck-engine соответствует последнему коммиту апстрима, трогающему rust/. =="
  exit 0
fi

echo "== АПСТРИМ УШЁЛ ВПЕРЁД =="
echo "Пин:      $PINNED"
echo "Апстрим:  $LATEST"
echo
echo "== Коммиты rust/ между пином и апстримом: =="
git log --oneline --format="%h %ad %s" --date=short "$PINNED..$LATEST" -- rust/ || true
echo
echo "Дальше: см. 'Как обновить пин' в luck-pilot/vendor/VENDOR.md"
exit 1
