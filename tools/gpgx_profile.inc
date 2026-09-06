/* nes-to-sms observational profiler, private ABI 1.
 * Included only by an isolated, modified GPGX z80.c; not a CPU implementation.
 * No guest writes, timing changes, or instruction replacement are performed.
 */
#include <stdint.h>
#include <inttypes.h>

#define NP_CAPACITY 262144
#define NP_DEPTH 64
#define NP_TICKS 65536
typedef struct {
  uint32_t version, tick_offset, initial_tick, start_tick, end_tick, predicate_count;
  uint32_t offsets[8], values[8];
} np_config;
typedef struct { uint64_t instructions, nominal_master, scaled_master; } np_cost;
typedef struct { uint64_t key; np_cost cost; } np_bucket;
typedef struct { uint32_t kind, pc, sp; } np_irq;
static np_config np_cfg;
static np_bucket np_buckets[NP_CAPACITY];
static np_cost np_ticks[NP_TICKS], np_totals;
static np_irq np_stack[NP_DEPTH], np_open[NP_DEPTH], np_close[NP_DEPTH];
static uint32_t np_depth, np_open_depth, np_close_depth, np_max_depth;
static uint32_t np_state, np_tick, np_raw, np_errors, np_event_active;
static uint32_t np_before_cycles;
static uint64_t np_event_scaled, np_key;
static uint64_t np_irq_entries, np_nmi_entries, np_halt_repeats;
/* state: 0 unarmed, 1 waiting, 2 collecting, 3 complete, 4 invalid. */

static void np_error(uint32_t bit) { np_errors |= bit; }

static uint64_t np_identity(uint32_t kind)
{
  uint32_t source = 2, offset = 0;
  uintptr_t p = (uintptr_t)z80_readmap[PC >> 10] + (PC & 1023);
  uintptr_t rom = (uintptr_t)cart.rom, ram = (uintptr_t)work_ram;
  if (p >= rom && p - rom < cart.romsize) { source = 0; offset = (uint32_t)(p - rom); }
  else if (p >= ram && p - ram < sizeof(work_ram)) source = 1;
  return (uint64_t)PC | ((uint64_t)offset << 16) | ((uint64_t)source << 42)
    | ((uint64_t)kind << 44) | ((uint64_t)(np_depth ? np_stack[np_depth-1].kind : 0) << 47)
    | ((uint64_t)np_depth << 49);
}

static np_cost *np_find(uint64_t key)
{
  uint32_t i, slot = (uint32_t)((key ^ (key >> 29)) * UINT64_C(11400714819323198485)) & (NP_CAPACITY-1);
  for (i = 0; i < NP_CAPACITY; i++, slot = (slot+1) & (NP_CAPACITY-1)) {
    if (!np_buckets[slot].key) np_buckets[slot].key = key + 1;
    if (np_buckets[slot].key == key + 1) return &np_buckets[slot].cost;
  }
  np_error(1); return NULL;
}

static void np_add(np_cost *c, uint64_t instructions, uint64_t nominal, uint64_t scaled)
{
  c->instructions += instructions; c->nominal_master += nominal; c->scaled_master += scaled;
}

static void np_record(uint64_t key, uint64_t instructions, uint64_t nominal, uint64_t scaled)
{
  np_cost *bucket;
  if (np_state != 2) return;
  bucket = np_find(key);
  if (bucket) np_add(bucket, instructions, nominal, scaled);
  np_add(&np_totals, instructions, nominal, scaled);
  np_add(&np_ticks[np_tick - np_cfg.start_tick], instructions, nominal, scaled);
}

static void np_begin(uint32_t kind)
{
  if (np_event_active) np_error(2);
  np_event_active = 1; np_event_scaled = 0; np_before_cycles = Z80.cycles;
  np_key = np_state == 2 ? np_identity(kind) : 0;
  if (np_state == 2) {
    np_record(np_key, kind < 2, 0, 0);
    if (kind == 1) np_halt_repeats++;
    if (kind == 2) np_irq_entries++;
    if (kind == 3) np_nmi_entries++;
  }
}

static void np_charge(uint32_t nominal, uint32_t scaled)
{
  if (np_event_active) {
    np_event_scaled += scaled;
    np_record(np_key, 0, nominal, scaled);
  } else np_record(np_identity(4), 0, nominal, scaled);
}

static void np_end(void)
{
  uint32_t raw, delta, i;
  if ((uint32_t)(Z80.cycles - np_before_cycles) != np_event_scaled) np_error(4);
  np_event_active = 0;
  if (np_state != 1 && np_state != 2) return;
  for (i = 0; i < np_cfg.predicate_count; i++) {
    if (work_ram[np_cfg.offsets[i]] != np_cfg.values[i]) { np_error(8); np_state = 4; return; }
  }
  raw = work_ram[np_cfg.tick_offset]; delta = (raw - np_raw) & 255;
  if (delta > 1) { np_error(16); np_state = 4; return; }
  np_raw = raw;
  if (!delta) return;
  np_tick++;
  if (np_tick == np_cfg.start_tick) {
    np_state = 2; np_open_depth = np_depth;
    memcpy(np_open, np_stack, sizeof(np_stack));
  }
  if (np_tick == np_cfg.end_tick) {
    np_state = 3; np_close_depth = np_depth;
    memcpy(np_close, np_stack, sizeof(np_stack));
  }
}

static void np_enter(uint32_t kind)
{
  if (np_depth == NP_DEPTH) { np_error(32); return; }
  np_stack[np_depth].kind = kind;
  np_stack[np_depth].pc = (PC + (HALT ? 1 : 0)) & 65535;
  np_stack[np_depth].sp = SP;
  np_depth++;
  if (np_depth > np_max_depth) np_max_depth = np_depth;
}

static void np_return(uint32_t kind)
{
  np_irq *frame;
  if (!np_depth) { if (kind) np_error(64); return; }
  frame = &np_stack[np_depth-1];
  /* The SMS runtime legitimately uses EI; RET, not RETI, to return from INT.
   * Ordinary nested CALL returns must not consume the hardware frame. */
  if (SP == frame->sp) {
    if (PC != frame->pc || (kind && kind != frame->kind)) np_error(128);
    else np_depth--;
  } else if (kind) np_error(128);
}

static void np_reset(void)
{
  np_state = np_depth = np_errors = np_event_active = np_max_depth = 0;
}

uint32_t retro_profile_version(void) { return 1; }

int retro_profile_arm(const np_config *cfg, size_t size)
{
  uint32_t i;
  if (!cfg || size != sizeof(np_config) || cfg->version != 1 || np_state ||
      cfg->tick_offset >= 8192 || cfg->initial_tick > 255 || !cfg->start_tick ||
      cfg->end_tick <= cfg->start_tick || cfg->end_tick-cfg->start_tick > NP_TICKS ||
      cfg->predicate_count > 8 || np_event_active) return -1;
  for (i=0; i<cfg->predicate_count; i++)
    if (cfg->offsets[i] >= 8192 || cfg->values[i] > 255) return -1;
  if (work_ram[cfg->tick_offset] != cfg->initial_tick) return -1;
  np_cfg = *cfg; np_raw = cfg->initial_tick; np_tick = 0;
  np_open_depth = np_close_depth = 0;
  np_irq_entries = np_nmi_entries = np_halt_repeats = 0;
  memset(np_buckets, 0, sizeof(np_buckets)); memset(np_ticks, 0, sizeof(np_ticks));
  memset(&np_totals, 0, sizeof(np_totals));
  /* Deliberately preserve interrupt frames and pre-window tracking errors. */
  np_state = 1; return 0;
}

static void np_print_cost(FILE *f, const np_cost *cost)
{
  fprintf(f, "\"instructions\":%" PRIu64 ",\"nominal_master\":%" PRIu64
          ",\"scaled_master\":%" PRIu64, cost->instructions, cost->nominal_master, cost->scaled_master);
}

static void np_print_stack(FILE *f, const np_irq *stack, uint32_t depth)
{
  uint32_t i;
  fputc('[', f);
  for (i=0; i<depth; i++) fprintf(f, "%s{\"kind\":\"%s\",\"return_pc\":%u,\"return_sp\":%u}",
    i ? "," : "", stack[i].kind == 1 ? "irq" : "nmi", stack[i].pc, stack[i].sp);
  fputc(']', f);
}

int retro_profile_dump(const char *path)
{
  FILE *f;
  uint32_t i, first = 1;
  const char *states[] = {"unarmed", "waiting", "collecting", "complete", "invalid"};
  const char *sources[] = {"rom", "ram", "unknown"};
  const char *kinds[] = {"instruction", "halt", "irq_entry", "nmi_entry", "external"};
  const char *irqs[] = {"none", "irq", "nmi"};
  const char *errors[] = {"bucket capacity", "nested host event", "scaled cycle residual",
    "stage predicate changed", "tick discontinuity", "interrupt depth capacity", "unmatched interrupt return", "interrupt return identity"};
  if (!path || np_event_active || !(f = fopen(path, "w"))) return -1;
  fprintf(f, "{\"schema\":1,\"window\":{\"start_tick\":%u,\"end_tick\":%u,\"observed_tick\":%u,\"state\":\"%s\"},\"errors\":[",
    np_cfg.start_tick, np_cfg.end_tick, np_tick, states[np_state]);
  for (i=0; i<8; i++) if (np_errors & (1u << i)) { fprintf(f, "%s\"%s\"", first ? "" : ",", errors[i]); first=0; }
  fprintf(f, "],\"totals\":{"); np_print_cost(f, &np_totals);
  fprintf(f, "},\"interrupts\":{\"max_depth\":%u,\"irq_entries\":%" PRIu64 ",\"nmi_entries\":%" PRIu64
    ",\"halt_repeats\":%" PRIu64 ",\"open\":", np_max_depth, np_irq_entries, np_nmi_entries, np_halt_repeats);
  np_print_stack(f, np_open, np_open_depth); fprintf(f, ",\"close\":"); np_print_stack(f, np_close, np_close_depth);
  fprintf(f, "},\"costs\":[\n"); first = 1;
  for (i=0; i<NP_CAPACITY; i++) if (np_buckets[i].key) {
    uint64_t key = np_buckets[i].key-1;
    uint32_t source = (key >> 42) & 3;
    fprintf(f, "%s{\"source\":\"%s\",\"rom_offset\":", first ? "" : ",\n", sources[source]);
    if (!source) fprintf(f, "%u", (uint32_t)((key >> 16) & 0x3ffffff)); else fprintf(f, "null");
    fprintf(f, ",\"pc\":%u,\"irq_kind\":\"%s\",\"irq_depth\":%u,\"kind\":\"%s\",",
      (uint32_t)(key & 65535), irqs[(key >> 47) & 3], (uint32_t)(key >> 49), kinds[(key >> 44) & 7]);
    np_print_cost(f, &np_buckets[i].cost); fputc('}', f); first = 0;
  }
  fprintf(f, "],\"ticks\":[\n");
  for (i=np_cfg.start_tick; i<np_tick && i<np_cfg.end_tick; i++) {
    fprintf(f, "%s{\"tick\":%u,", i == np_cfg.start_tick ? "" : ",\n", i);
    np_print_cost(f, &np_ticks[i-np_cfg.start_tick]); fputc('}', f);
  }
  fprintf(f, "]}\n");
  { int failed = ferror(f); return fclose(f) || failed ? -1 : 0; }
}
