#!/bin/bash
ROM="/mnt/terachad/Emulators/EmuDeck/roms/nes/Castlevania (USA) (Rev 1).nes"
for i in $(seq 1 40); do
  OUT=$(target/release/trace-sms out/cv1/sms.sms --steps 40000000 2>&1)
  MARK=$(echo "$OUT" | sed -n 's/.*trap_marker=\$\(E[0-9A-F]\).*/\1/p' | head -1)
  if [ -z "$MARK" ] || [ "$MARK" = "00" ]; then echo "ITER $i: NO TRAP — done"; break; fi
  if [ "$MARK" = "E2" ]; then
    ADDR=$(echo "$OUT" | sed -n 's/.*unresolved_id=\$\([0-9A-F]*\).*/\1/p' | head -1)
    # byte order: $CB1B=lo $CB1C=hi printed as id -> id IS hi<<8|lo? diagnostics print = (CB1C<<8)|CB1B
    HI=${ADDR:0:2}; LO=${ADDR:2:2}
    # unresolved_id prints as 16-bit: value = CB1C:CB1B = hi:lo -> target = $HI$LO
    TGT="$HI$LO"
    BANK=$(echo "$OUT" | sed -n 's/.*first trap.*nes_bank=\([0-9]*\).*/\1/p' | head -1)
    if [ $((16#$HI)) -ge $((16#C0)) ]; then
      echo "ITER $i: E2 miss fixed \$$TGT -> [[function]]"
      printf '\n[[function]]\naddr = 0x%s\nname = "func_%s"\n' "${TGT,,}" "$TGT" >> profiles/cv1.toml
    else
      NB=${BANK:-0}
      echo "ITER $i: E2 miss window \$$TGT bank=$NB -> [[bank_entry]]"
      printf '\n[[bank_entry]]\nbank = %d\naddr = 0x%s\n' "$NB" "${TGT,,}" >> profiles/cv1.toml
    fi
  else
    T=$(echo "$OUT" | grep "first trap" | head -1)
    ID=$(echo "$T" | sed -n 's/.*unresolved_id=\$\([0-9A-F]*\).*/\1/p')
    BANK=$(( $(echo "$T" | sed -n 's/.*slot2_bank=\([0-9]*\).*/\1/p') - 36 )); [ $BANK -lt 0 ] && BANK=0
    IDX=$((16#$ID))
    LBL=$(awk -v n=$IDX 'NR==n+1 {print $2}' out/cv1/reports/unresolved_labels.txt)
    ADDR=${LBL#L_}; ADDR=${ADDR#b*_}
    if [ $((16#${ADDR:0:2})) -ge $((16#C0)) ]; then
      grep -q "addr = 0x${ADDR,,}" profiles/cv1.toml && { echo "ITER $i: $LBL already annotated — stuck, abort"; break; }
      echo "ITER $i: E1 fixed $LBL -> [[function]]"
      printf '\n[[function]]\naddr = 0x%s\nname = "func_%s"\n' "${ADDR,,}" "$ADDR" >> profiles/cv1.toml
    else
      grep -q "bank = $BANK\naddr = 0x${ADDR,,}" profiles/cv1.toml 2>/dev/null && { echo "ITER $i: repeat annotation — stuck, abort"; break; }
      echo "ITER $i: E1 unresolved $LBL bank=$BANK -> annotating 0x${ADDR,,}"
      printf '\n[[bank_entry]]\nbank = %d\naddr = 0x%s\n' "$BANK" "${ADDR,,}" >> profiles/cv1.toml
    fi
  fi
  cargo run --release -p nes_to_sms --bin nes-to-sms -- "$ROM" profiles/cv1.toml out/cv1 --runtime runtime >/dev/null 2>&1 || { echo "REGEN FAIL"; break; }
  docker compose run --rm --user root --workdir /work poc bash -lc 'cd out/cv1 && rm -f obj/sms.o sms.sms && make >/dev/null 2>&1 && echo ok' 2>/dev/null | grep -q ok || { echo "BUILD FAIL at iter $i"; break; }
done
target/release/trace-sms out/cv1/sms.sms --steps 80000000 2>&1 | grep -E "trap_marker|ppu_mask|IRQs fired|VRAM writes|nonzero"
