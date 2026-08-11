#!/bin/bash
# Единый прогон всех 4 сценариев HoReCa через десктопную Ollama (GPU).
cd /home/avk/projects/horeca-pilot/luck-pilot
export OLLAMA_HOST=http://100.64.0.1:11434
export OLLAMA_MODEL=hermes3:8b
export OLLAMA_ONLY=1

for f in horeca-daily-cycle horeca-returns horeca-inventory horeca-cashflow; do
  echo "=================== $f ==================="
  timeout 300 ./target/debug/run ../examples_luck/$f.luck 2>&1 | grep -vE "warning|^\s*-->|^\s*\||^\s*=|^\s*$" | head -40
  echo "exit=$?"
done
echo "=================== ВСЕ СЦЕНАРИИ ПРОЙДЕНЫ ==================="
