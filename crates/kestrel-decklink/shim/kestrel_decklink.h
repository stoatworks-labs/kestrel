// A C ABI over the Blackmagic DeckLink SDK.
//
// The SDK is C++ with COM-shaped lifetimes and callback interfaces, none of
// which crosses to Rust cleanly. This header is the whole boundary: plain
// functions, opaque handles, POD structs. Everything above it in Rust is safe
// code; everything below it is the SDK's own idiom, kept in one file.
//
// The SDK itself is a free but licence-gated download and is not ours to
// redistribute, so nothing from it is vendored. Point the build at a copy with
// DECKLINK_SDK_DIR. Without one the crate still builds — see `unavailable.rs`
// — and reports that DeckLink was not compiled in, which is a different thing
// from "no card found" and is reported as such.

#ifndef KESTREL_DECKLINK_H
#define KESTREL_DECKLINK_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define KD_NAME_MAX 128

// Return codes. Negative is failure; call kd_last_error for the text.
#define KD_OK 0
#define KD_ERR_NO_SDK (-1)
#define KD_ERR_NO_DEVICE (-2)
#define KD_ERR_BAD_MODE (-3)
#define KD_ERR_BUSY (-4)
#define KD_ERR_FAILED (-5)

typedef struct {
  // BMDDeckLinkPersistentID: stable across reboots and device reordering, so
  // it is what a show file remembers.
  int64_t persistent_id;
  char name[KD_NAME_MAX];
  // Index within the physical card (BMDDeckLinkSubDeviceIndex).
  int32_t sub_device;
  int32_t has_input;
  int32_t has_output;
  // Zero when the card's current *profile* has this sub-device switched off.
  //
  // This is not a detail. A DeckLink Duo 2 in its two-sub-device profile shows
  // four sub-devices of which two support no display modes at all — asking one
  // of those to open looks exactly like a broken card. Kestrel needs one input
  // and several outputs at once, which usually means the four-sub-device
  // half-duplex profile, so the UI has to be able to say why a port is dead.
  int32_t active;
  // Whether this sub-device does input and output at the same time.
  int32_t full_duplex;
} kd_device;

typedef struct {
  int64_t scheduled;
  int64_t completed;
  int64_t late;
  int64_t dropped;
  // What the card is holding. The number to watch: a value that walks steadily
  // up or down means our clock and the card's disagree.
  int32_t buffered;
} kd_output_stats;

// 1 when the shim was compiled against a real SDK. Always check this before
// blaming an empty device list on the hardware.
int32_t kd_available(void);

// Fills up to `max` entries, returns the count written, or negative on error.
int32_t kd_list_devices(kd_device* out, int32_t max);

// The last failure on this thread, or "".
const char* kd_last_error(void);

// --- capture --------------------------------------------------------------

// Called on an SDK thread, never on the caller's. `bytes` is valid only for the
// duration of the call.
//
// `no_signal` is set for frames the card synthesised because nothing is
// arriving. Those still tick at the nominal rate, which is why "frames are
// arriving" is not the same question as "a source is connected".
typedef void (*kd_frame_fn)(void* ctx, const uint8_t* bytes, int32_t row_bytes,
                            int32_t width, int32_t height, int64_t rate_num,
                            int64_t rate_den, int32_t no_signal);

// Opens with input format detection on, so the caller does not have to know
// the source format up front — the callback reports whatever arrives.
void* kd_capture_open(int64_t persistent_id, kd_frame_fn cb, void* ctx);
void kd_capture_close(void* handle);

// --- playback -------------------------------------------------------------

void* kd_output_open(int64_t persistent_id, int32_t width, int32_t height,
                     int64_t rate_num, int64_t rate_den, int32_t interlaced);

// Copies one UYVY frame into a card frame and schedules it. Returns KD_OK or a
// negative code.
int32_t kd_output_schedule(void* handle, const uint8_t* bytes,
                           int32_t row_bytes);

int32_t kd_output_stats_get(void* handle, kd_output_stats* out);
void kd_output_close(void* handle);

#ifdef __cplusplus
}
#endif

#endif  // KESTREL_DECKLINK_H
