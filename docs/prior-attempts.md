# Prior Attempts and Related Work

## Super Mario Bros. SMS Port POC

LackofTrack released a Super Mario Bros. Master System proof-of-concept port in
2026.

Relevant facts:

- Platform: Master System.
- Version observed in research: 0.12, dated 2026-04-01.
- Only the first level is playable.
- The original SMB disassembly by doppelganger was heavily used as a reference.
- The author describes the result as very close but not 100% accurate.
- FM sound support is planned, but not the baseline.

This is the strongest direct precedent. It confirms SMB-on-SMS is feasible, but
it also confirms that the realistic method is a porting workflow with extensive
reference to the original disassembly.

References:

- https://www.smspower.org/Homebrew/SuperMarioBrosSmsPort-SMS
- https://www.smspower.org/forums/20831-CodingCompetition2026SuperMarioBrosSMSPortPOCByLackofTrack

Important technical observation from the forum thread:

- The author had to process level data in narrower strips because SMS has one
  nametable in the relevant setup where NES SMB uses scrolling behavior tied to
  its nametable arrangement.
- The author also mentions manually translating many lines of 6502 assembly into
  Z80, with physics accuracy requiring careful review.

That is directly relevant to this project. It means a converter should generate
assistance and scaffolding, not assume whole-game correctness from raw opcode
translation.

## SMS Power: Porting NES Games to SMS

SMS Power has an explicit forum discussion about porting NES games to SMS. The
general conclusion is consistent with this research:

- Hardware I/O must be rewritten.
- Graphics and sound need platform-specific handling.
- CPU instruction translation is only one part of the work.
- A generic automated conversion would need so many per-game fixes that it
  becomes a porting tool rather than a turnkey converter.

Reference: https://www.smspower.org/forums/19447-PortingNESGamesToSMS

## SMS Power: Nes2Sms?

An older SMS Power thread discussed the idea of NES-to-SMS conversion/emulation.
It is useful as historical context, especially around performance and CPU
differences.

Reference: https://www.smspower.org/forums/3702-Nes2Sms

## Analogous Stronger-Hardware Projects

### SMB to Genesis

There are reports of Super Mario Bros. being ported to Genesis using an
automatic 6502-to-68000 assembly conversion path plus recreated hardware
functions.

This is relevant but not directly comparable:

- Genesis has much more CPU, memory, video, and audio headroom than SMS.
- A hardware shim approach is more forgiving on Genesis than on SMS.

Reference: https://www.techeblog.com/gamer-ports-super-mario-bros-to-the-sega-genesis/

### Project Nested

Project Nested runs NES games on SNES. This demonstrates that automated NES
compatibility layers are possible when the target machine has enough headroom.
The SMS does not have comparable headroom.

Reference: https://www.retrorgb.com/project-nested-play-nes-games-on-snes.html

## Related Converter: MSXtoSMS

MSXtoSMS converts MSX software to Master System. This is a useful precedent for
targeting SMS, but it is easier than NES-to-SMS because MSX and SMS are both
Z80/TMS-derived ecosystems.

Reference: https://segaretro.org/MSXtoSMS

