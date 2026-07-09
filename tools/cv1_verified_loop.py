import subprocess, re
d = open("/mnt/terachad/Emulators/EmuDeck/roms/nes/Castlevania (USA) (Rev 1).nes",'rb').read()
prg = d[16:16+131072]
JAM = {0x02,0x12,0x22,0x32,0x42,0x52,0x62,0x72,0x92,0xB2,0xD2,0xF2}
def code_ok(bank, addr):
    off = (len(prg)-0x4000 + (addr-0xC000)) if addr >= 0xC000 else bank*0x4000 + (addr-0x8000)
    b = prg[off]
    return b not in JAM and b not in (0x00, 0xFF)
harvest = set()
for line in open('/tmp/cv1_be3.txt'):
    m = re.match(r"BANK_ENTRY bank=(\d+) addr=0x([0-9a-f]+)", line)
    if m: harvest.add((int(m.group(1)), int(m.group(2),16)))
ROM = "/mnt/terachad/Emulators/EmuDeck/roms/nes/Castlevania (USA) (Rev 1).nes"
for i in range(40):
    r = subprocess.run(["target/release/trace-sms","out/cv1/sms.sms","--steps","300000000"],
                       capture_output=True, text=True)
    out = r.stdout + r.stderr
    mm = re.search(r"trap_marker=\$(E[0-9A-F])", out)
    if not mm:
        print(f"iter {i}: NO TRAP — done"); break
    marker = mm.group(1)
    mid = re.search(r"unresolved_id=\$([0-9A-F]{4})", out)
    mb = re.search(r"nes_bank=(\d+)", out)
    idv = int(mid.group(1), 16)
    if marker == "E1":
        # index into the unresolved list
        lines = open('out/cv1/reports/unresolved_labels.txt').read().splitlines()
        lbl = lines[idv].split()[1]
        core = lbl.split('_')[-1]
        addr = int(core, 16)
        bank = int(lbl.split('_')[1][1:]) if lbl.startswith('L_b') else None
    else:  # E2: address; bank from live shadow
        addr = idv
        bank = int(mb.group(1)) if mb else None
    if addr >= 0xC000:
        if not code_ok(0, addr):
            print(f"iter {i}: fixed ${addr:04X} looks like DATA — manual stop"); break
        print(f"iter {i}: {marker} fixed ${addr:04X} -> [[function]]")
        with open('profiles/cv1.toml','a') as f:
            f.write(f'\n[[function]]\naddr = 0x{addr:04x}\nname = "func_{addr:04X}"\n')
    else:
        b = bank if bank is not None else 0
        tag = "harvest" if (b,addr) in harvest else ("codeish" if code_ok(b,addr) else "DATA")
        if tag == "DATA":
            print(f"iter {i}: (b{b}, ${addr:04X}) DATA — manual stop"); break
        print(f"iter {i}: {marker} (b{b}, ${addr:04X}) {tag} -> [[bank_entry]]")
        with open('profiles/cv1.toml','a') as f:
            f.write(f"\n[[bank_entry]]\nbank = {b}\naddr = 0x{addr:04x}\n")
    subprocess.run(["cargo","run","--release","-p","nes_to_sms","--bin","nes-to-sms","--",
                    ROM,"profiles/cv1.toml","out/cv1","--runtime","runtime"], capture_output=True)
    br = subprocess.run(["docker","compose","run","--rm","--user","root","--workdir","/work","poc",
                        "bash","-lc","cd out/cv1 && rm -f obj/sms.o sms.sms && make >/dev/null 2>&1 && echo ok"],
                       capture_output=True, text=True)
    if "ok" not in br.stdout:
        print(f"iter {i}: BUILD FAIL — manual stop"); break
