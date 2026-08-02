// The DeckLink side of the C ABI.
//
// Two things are load-bearing here and both were learned on real hardware in a
// sibling project (weblinked, docs/04-verification.md section 19):
//
//   * Scheduled playback, not DisplayVideoFrameSync. Scheduled playback is the
//     only mode that gives genlocked, correctly timed SDI while being driven
//     by a software clock. Pre-roll a few frames, start, then top the queue up.
//     Measured over two minutes at 1080p50: buffered held at 6, zero drops.
//   * Display times come from a monotonically increasing frame *index*, never
//     from wall-clock arithmetic. A late tick then shortens the queue instead
//     of scheduling a frame in the past, which the card rejects outright.
//
// COM rules apply: everything the API hands back is Release()d.

#include "kestrel_decklink.h"

#include <atomic>
#include <cstring>
#include <mutex>
#include <string>
#include <vector>

#include "DeckLinkAPI.h"

namespace {

thread_local std::string g_error;

void set_error(const std::string& msg) { g_error = msg; }

template <typename T>
void safe_release(T*& obj) {
  if (obj != nullptr) {
    obj->Release();
    obj = nullptr;
  }
}

// The SDK returns a different string type per platform, and each has its own
// ownership rule. All three leak if you guess.
#if defined(_WIN32)
std::string to_std(BSTR text) {
  if (!text) return {};
  const int len = ::SysStringLen(text);
  const int bytes =
      ::WideCharToMultiByte(CP_UTF8, 0, text, len, nullptr, 0, nullptr, nullptr);
  std::string out(static_cast<size_t>(bytes), '\0');
  ::WideCharToMultiByte(CP_UTF8, 0, text, len, out.data(), bytes, nullptr,
                        nullptr);
  ::SysFreeString(text);
  return out;
}
using DeckLinkBool = BOOL;
#elif defined(__APPLE__)
std::string to_std(CFStringRef text) {
  if (!text) return {};
  const CFIndex len = CFStringGetLength(text);
  const CFIndex max =
      CFStringGetMaximumSizeForEncoding(len, kCFStringEncodingUTF8) + 1;
  std::string buf(static_cast<size_t>(max), '\0');
  std::string out;
  if (CFStringGetCString(text, buf.data(), max, kCFStringEncodingUTF8)) {
    out = buf.c_str();
  }
  CFRelease(text);
  return out;
}
using DeckLinkBool = bool;
#else
std::string to_std(const char* text) {
  if (!text) return {};
  std::string out(text);
  ::free(const_cast<char*>(text));  // the Linux SDK returns malloc'd strings
  return out;
}
using DeckLinkBool = bool;
#endif

// Microseconds. Fine for every rate in use — 1001/60000 s is 16683 whole
// microseconds — and it is what the sibling project's soak test ran at.
constexpr BMDTimeScale kTimeScale = 1'000'000;

// Deep enough to ride out a scheduling hiccup, shallow enough not to add
// visible latency. Three frames is 60 ms at 50p. Measured steady state on a
// Duo 2 was six buffered with this pre-roll.
constexpr int kPreroll = 3;

void copy_error_from(const char* what, HRESULT hr) {
  set_error(std::string(what) + " failed (hr=" +
            std::to_string(static_cast<long long>(hr)) + ")");
}

/// Walks the device list looking for a persistent id. Returns a device with a
/// reference the caller owns, or nullptr.
IDeckLink* find_device(int64_t persistent_id) {
  IDeckLinkIterator* it = CreateDeckLinkIteratorInstance();
  if (it == nullptr) {
    set_error(
        "no DeckLink driver. The SDK headers are compiled in, but Desktop "
        "Video is not installed or not running on this machine.");
    return nullptr;
  }
  IDeckLink* device = nullptr;
  IDeckLink* found = nullptr;
  while (it->Next(&device) == S_OK) {
    IDeckLinkProfileAttributes* attrs = nullptr;
    int64_t id = 0;
    if (device->QueryInterface(IID_IDeckLinkProfileAttributes,
                               reinterpret_cast<void**>(&attrs)) == S_OK) {
      attrs->GetInt(BMDDeckLinkPersistentID, &id);
      safe_release(attrs);
    }
    if (id == persistent_id) {
      found = device;  // keep our reference
      break;
    }
    safe_release(device);
  }
  safe_release(it);
  if (found == nullptr) {
    set_error("no DeckLink sub-device with persistent id " +
              std::to_string(static_cast<long long>(persistent_id)));
  }
  return found;
}

/// Finds the display mode matching a raster and an exact rational rate.
///
/// Matched by asking the card what it has rather than by mapping to a
/// BMDDisplayMode constant: the constant set differs between SDK versions, and
/// a card's supported set differs between profiles.
BMDDisplayMode find_mode(IDeckLinkOutput* output, IDeckLinkInput* input,
                         int32_t width, int32_t height, int64_t rate_num,
                         int64_t rate_den, bool interlaced) {
  IDeckLinkDisplayModeIterator* it = nullptr;
  HRESULT hr = output != nullptr ? output->GetDisplayModeIterator(&it)
                                 : input->GetDisplayModeIterator(&it);
  if (hr != S_OK || it == nullptr) return bmdModeUnknown;

  BMDDisplayMode result = bmdModeUnknown;
  IDeckLinkDisplayMode* mode = nullptr;
  while (it->Next(&mode) == S_OK) {
    BMDTimeValue duration = 0;
    BMDTimeScale scale = 0;
    const bool size_ok = mode->GetWidth() == width && mode->GetHeight() == height;
    const BMDFieldDominance fd = mode->GetFieldDominance();
    const bool is_interlaced = fd == bmdLowerFieldFirst || fd == bmdUpperFieldFirst;
    if (size_ok && is_interlaced == interlaced &&
        mode->GetFrameRate(&duration, &scale) == S_OK) {
      // Compare as a cross-multiplied rational. 59.94 is 60000/1001 and the
      // card may report it as 1001/60000 or 2002/120000; a float compare with
      // an epsilon gets 59.94 and 60 confused often enough to matter.
      if (static_cast<__int128>(scale) * rate_den ==
          static_cast<__int128>(duration) * rate_num) {
        result = mode->GetDisplayMode();
      }
    }
    safe_release(mode);
    if (result != bmdModeUnknown) break;
  }
  safe_release(it);
  return result;
}

// --- capture --------------------------------------------------------------

class Capture final : public IDeckLinkInputCallback {
 public:
  Capture(kd_frame_fn cb, void* ctx) : cb_(cb), ctx_(ctx) {}

  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID, void**) override {
    return E_NOINTERFACE;
  }
  ULONG STDMETHODCALLTYPE AddRef() override { return ++refs_; }
  ULONG STDMETHODCALLTYPE Release() override {
    const ULONG n = --refs_;
    if (n == 0) delete this;
    return n;
  }

  /// The card telling us the source changed. Restarting the streams with the
  /// new mode is the whole reason format detection is worth enabling: an
  /// operator plugs in whatever the camera department gave them.
  HRESULT STDMETHODCALLTYPE VideoInputFormatChanged(
      BMDVideoInputFormatChangedEvents, IDeckLinkDisplayMode* mode,
      BMDDetectedVideoInputFormatFlags) override {
    if (mode == nullptr || input_ == nullptr) return S_OK;
    input_->PauseStreams();
    input_->EnableVideoInput(mode->GetDisplayMode(), bmdFormat8BitYUV,
                             bmdVideoInputEnableFormatDetection);
    input_->FlushStreams();
    input_->StartStreams();
    return S_OK;
  }

  HRESULT STDMETHODCALLTYPE VideoInputFrameArrived(
      IDeckLinkVideoInputFrame* frame, IDeckLinkAudioInputPacket*) override {
    if (frame == nullptr || cb_ == nullptr) return S_OK;
    void* bytes = nullptr;
    if (frame->GetBytes(&bytes) != S_OK || bytes == nullptr) return S_OK;

    BMDTimeValue duration = 0;
    BMDTimeScale scale = 0;
    // A frame with no source still has a nominal rate; report it anyway so the
    // caller sees a consistent shape and decides on `no_signal` alone.
    frame->GetStreamTime(nullptr, nullptr, kTimeScale);
    IDeckLinkDisplayMode* dm = nullptr;
    int64_t num = 0, den = 0;
    if (input_ != nullptr && frame->QueryInterface(IID_IDeckLinkDisplayMode,
                                                   reinterpret_cast<void**>(&dm)) == S_OK) {
      if (dm->GetFrameRate(&duration, &scale) == S_OK) {
        num = scale;
        den = duration;
      }
      safe_release(dm);
    }

    const int32_t no_signal =
        (frame->GetFlags() & bmdFrameHasNoInputSource) ? 1 : 0;
    cb_(ctx_, static_cast<const uint8_t*>(bytes), frame->GetRowBytes(),
        frame->GetWidth(), frame->GetHeight(), num, den, no_signal);
    return S_OK;
  }

  bool open(int64_t persistent_id) {
    device_ = find_device(persistent_id);
    if (device_ == nullptr) return false;
    if (device_->QueryInterface(IID_IDeckLinkInput,
                                reinterpret_cast<void**>(&input_)) != S_OK) {
      set_error("this sub-device has no input");
      return false;
    }
    if (input_->SetCallback(this) != S_OK) {
      set_error("SetCallback failed on the input");
      return false;
    }
    // Start on a mode the card certainly has; detection replaces it with
    // whatever is really arriving on the first format-changed event.
    HRESULT hr = input_->EnableVideoInput(bmdModeHD1080p50, bmdFormat8BitYUV,
                                          bmdVideoInputEnableFormatDetection);
    if (hr != S_OK) {
      copy_error_from("EnableVideoInput", hr);
      return false;
    }
    if (input_->StartStreams() != S_OK) {
      set_error("StartStreams failed on the input");
      return false;
    }
    return true;
  }

  void close() {
    if (input_ != nullptr) {
      input_->StopStreams();
      input_->SetCallback(nullptr);
      input_->DisableVideoInput();
    }
    safe_release(input_);
    safe_release(device_);
  }

 private:
  ~Capture() { close(); }

  std::atomic<ULONG> refs_{1};
  kd_frame_fn cb_ = nullptr;
  void* ctx_ = nullptr;
  IDeckLink* device_ = nullptr;
  IDeckLinkInput* input_ = nullptr;
};

// --- playback -------------------------------------------------------------

class Playback;

/// Hands finished frames back to the pool and counts what went wrong.
class Completion final : public IDeckLinkVideoOutputCallback {
 public:
  explicit Completion(Playback* owner) : owner_(owner) {}

  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID, void**) override {
    return E_NOINTERFACE;
  }
  ULONG STDMETHODCALLTYPE AddRef() override { return ++refs_; }
  ULONG STDMETHODCALLTYPE Release() override {
    const ULONG n = --refs_;
    if (n == 0) delete this;
    return n;
  }

  HRESULT STDMETHODCALLTYPE ScheduledFrameCompleted(
      IDeckLinkVideoFrame* frame, BMDOutputFrameCompletionResult result) override;

  HRESULT STDMETHODCALLTYPE ScheduledPlaybackHasStopped() override {
    return S_OK;
  }

 private:
  std::atomic<ULONG> refs_{1};
  Playback* owner_;
};

class Playback {
 public:
  bool open(int64_t persistent_id, int32_t width, int32_t height,
            int64_t rate_num, int64_t rate_den, bool interlaced) {
    width_ = width;
    height_ = height;
    rate_num_ = rate_num;
    rate_den_ = rate_den;

    device_ = find_device(persistent_id);
    if (device_ == nullptr) return false;
    if (device_->QueryInterface(IID_IDeckLinkOutput,
                                reinterpret_cast<void**>(&output_)) != S_OK) {
      set_error("this sub-device has no output");
      return false;
    }

    mode_ = find_mode(output_, nullptr, width, height, rate_num, rate_den,
                      interlaced);
    if (mode_ == bmdModeUnknown) {
      set_error(
          "this sub-device does not offer " + std::to_string(width) + "x" +
          std::to_string(height) + " at " + std::to_string(rate_num) + "/" +
          std::to_string(rate_den) +
          ". On a multi-sub-device card an empty mode list usually means the "
          "card's profile has this sub-device switched off rather than that "
          "the format is unsupported.");
      return false;
    }

    HRESULT hr = output_->EnableVideoOutput(mode_, bmdVideoOutputFlagDefault);
    if (hr != S_OK) {
      copy_error_from("EnableVideoOutput", hr);
      return false;
    }

    completion_ = new Completion(this);
    if (output_->SetScheduledFrameCompletionCallback(completion_) != S_OK) {
      set_error("SetScheduledFrameCompletionCallback failed");
      return false;
    }

    // Pre-roll black so the output is carrying legal video from the moment it
    // starts, rather than whatever the card happened to have in its buffer.
    for (int i = 0; i < kPreroll; ++i) {
      IDeckLinkMutableVideoFrame* frame = acquire();
      if (frame == nullptr) return false;
      fill_black(frame);
      schedule(frame);
    }

    if (output_->StartScheduledPlayback(0, kTimeScale, 1.0) != S_OK) {
      set_error("StartScheduledPlayback failed");
      return false;
    }
    running_ = true;
    return true;
  }

  int32_t push(const uint8_t* bytes, int32_t row_bytes) {
    if (!running_) return KD_ERR_FAILED;
    IDeckLinkMutableVideoFrame* frame = acquire();
    if (frame == nullptr) return KD_ERR_FAILED;

    void* dst = nullptr;
    if (frame->GetBytes(&dst) != S_OK || dst == nullptr) {
      recycle(frame);
      return KD_ERR_FAILED;
    }
    const int32_t card_row = frame->GetRowBytes();
    if (card_row == row_bytes) {
      std::memcpy(dst, bytes, static_cast<size_t>(row_bytes) * height_);
    } else {
      // The card's stride need not match ours. Copying the shorter of the two
      // per row is what keeps a stride mismatch a non-event instead of a
      // buffer overrun.
      const int32_t n = card_row < row_bytes ? card_row : row_bytes;
      for (int32_t y = 0; y < height_; ++y) {
        std::memcpy(static_cast<uint8_t*>(dst) + static_cast<size_t>(y) * card_row,
                    bytes + static_cast<size_t>(y) * row_bytes,
                    static_cast<size_t>(n));
      }
    }
    schedule(frame);
    return KD_OK;
  }

  void stats(kd_output_stats* out) {
    out->scheduled = scheduled_;
    out->completed = completed_;
    out->late = late_;
    out->dropped = dropped_;
    uint32_t buffered = 0;
    if (output_ != nullptr &&
        output_->GetBufferedVideoFrameCount(&buffered) == S_OK) {
      out->buffered = static_cast<int32_t>(buffered);
    } else {
      out->buffered = -1;
    }
  }

  void close() {
    running_ = false;
    if (output_ != nullptr) {
      BMDTimeValue stopped = 0;
      output_->StopScheduledPlayback(0, &stopped, kTimeScale);
      output_->SetScheduledFrameCompletionCallback(nullptr);
      output_->DisableVideoOutput();
    }
    {
      std::lock_guard<std::mutex> lock(pool_lock_);
      for (auto* f : pool_) f->Release();
      pool_.clear();
    }
    safe_release(completion_);
    safe_release(output_);
    safe_release(device_);
  }

  /// Called from the completion callback, on an SDK thread.
  void completed(IDeckLinkVideoFrame* frame, BMDOutputFrameCompletionResult r) {
    ++completed_;
    if (r == bmdOutputFrameDisplayedLate) ++late_;
    if (r == bmdOutputFrameDropped) ++dropped_;
    recycle(static_cast<IDeckLinkMutableVideoFrame*>(frame));
  }

 private:
  /// Take a frame from the pool, or make one.
  ///
  /// Pooled rather than created per frame because Kestrel runs several outputs
  /// at once: at four HD outputs and 50p that is 200 frame allocations a
  /// second, about 800 MB/s of churn, for buffers that are all identical.
  IDeckLinkMutableVideoFrame* acquire() {
    {
      std::lock_guard<std::mutex> lock(pool_lock_);
      if (!pool_.empty()) {
        IDeckLinkMutableVideoFrame* f = pool_.back();
        pool_.pop_back();
        return f;
      }
    }
    IDeckLinkMutableVideoFrame* frame = nullptr;
    const int32_t row = ((width_ + 1) / 2) * 4;
    if (output_->CreateVideoFrame(width_, height_, row, bmdFormat8BitYUV,
                                  bmdFrameFlagDefault, &frame) != S_OK) {
      set_error("CreateVideoFrame failed");
      return nullptr;
    }
    return frame;
  }

  void recycle(IDeckLinkMutableVideoFrame* frame) {
    if (frame == nullptr) return;
    std::lock_guard<std::mutex> lock(pool_lock_);
    // Cap the pool. A card that stops completing frames must not be able to
    // grow this without limit.
    if (pool_.size() < 16) {
      pool_.push_back(frame);
    } else {
      frame->Release();
    }
  }

  void fill_black(IDeckLinkMutableVideoFrame* frame) {
    void* bytes = nullptr;
    if (frame->GetBytes(&bytes) != S_OK || bytes == nullptr) return;
    // Legal black in UYVY, not zeros: Y=16, C=128.
    uint8_t* p = static_cast<uint8_t*>(bytes);
    const size_t n = static_cast<size_t>(frame->GetRowBytes()) * height_;
    for (size_t i = 0; i + 3 < n; i += 4) {
      p[i] = 128;
      p[i + 1] = 16;
      p[i + 2] = 128;
      p[i + 3] = 16;
    }
  }

  void schedule(IDeckLinkMutableVideoFrame* frame) {
    const BMDTimeValue duration =
        (static_cast<BMDTimeValue>(kTimeScale) * rate_den_) / rate_num_;
    const BMDTimeValue when = scheduled_ * duration;
    if (output_->ScheduleVideoFrame(frame, when, duration, kTimeScale) != S_OK) {
      recycle(frame);
      return;
    }
    ++scheduled_;
    // The card holds its own reference now; ours comes back through the
    // completion callback, which is what keeps the buffer alive until the card
    // has finished reading it.
  }

  IDeckLink* device_ = nullptr;
  IDeckLinkOutput* output_ = nullptr;
  Completion* completion_ = nullptr;
  BMDDisplayMode mode_ = bmdModeUnknown;
  int32_t width_ = 0, height_ = 0;
  int64_t rate_num_ = 0, rate_den_ = 0;
  bool running_ = false;

  std::mutex pool_lock_;
  std::vector<IDeckLinkMutableVideoFrame*> pool_;

  std::atomic<int64_t> scheduled_{0};
  std::atomic<int64_t> completed_{0};
  std::atomic<int64_t> late_{0};
  std::atomic<int64_t> dropped_{0};
};

HRESULT STDMETHODCALLTYPE Completion::ScheduledFrameCompleted(
    IDeckLinkVideoFrame* frame, BMDOutputFrameCompletionResult result) {
  if (owner_ != nullptr) owner_->completed(frame, result);
  return S_OK;
}

}  // namespace

// --- the C ABI ------------------------------------------------------------

extern "C" {

int32_t kd_available(void) { return 1; }

const char* kd_last_error(void) { return g_error.c_str(); }

int32_t kd_list_devices(kd_device* out, int32_t max) {
  if (out == nullptr || max <= 0) return 0;
  IDeckLinkIterator* it = CreateDeckLinkIteratorInstance();
  if (it == nullptr) {
    set_error(
        "no DeckLink driver. The SDK headers are compiled in, but Desktop "
        "Video is not installed or not running on this machine.");
    return KD_ERR_NO_SDK;
  }

  int32_t n = 0;
  IDeckLink* device = nullptr;
  while (n < max && it->Next(&device) == S_OK) {
    kd_device d;
    std::memset(&d, 0, sizeof(d));
    bool inactive_by_duplex = false;

    const std::string name = [&] {
#if defined(_WIN32)
      BSTR s = nullptr;
#elif defined(__APPLE__)
      CFStringRef s = nullptr;
#else
      const char* s = nullptr;
#endif
      return device->GetDisplayName(&s) == S_OK ? to_std(s) : std::string("DeckLink");
    }();
    std::strncpy(d.name, name.c_str(), KD_NAME_MAX - 1);

    IDeckLinkProfileAttributes* attrs = nullptr;
    if (device->QueryInterface(IID_IDeckLinkProfileAttributes,
                               reinterpret_cast<void**>(&attrs)) == S_OK) {
      attrs->GetInt(BMDDeckLinkPersistentID, &d.persistent_id);
      int64_t sub = 0;
      attrs->GetInt(BMDDeckLinkSubDeviceIndex, &sub);
      d.sub_device = static_cast<int32_t>(sub);
      int64_t duplex = 0;
      if (attrs->GetInt(BMDDeckLinkDuplex, &duplex) == S_OK) {
        d.full_duplex = duplex == bmdDuplexFull ? 1 : 0;
        // The card's own word for "this profile has me switched off". Recorded
        // here and cross-checked against the display-mode probe below, because
        // not every card reports duplex and not every inactive sub-device
        // reports it the same way.
        inactive_by_duplex = duplex == bmdDuplexInactive;
      }
      safe_release(attrs);
    }

    // Whether a direction exists is answered by asking for the interface;
    // whether it is *usable in this profile* is answered by whether the card
    // offers any display mode on it. Both are reported, because a sub-device
    // that has an output interface and no modes is the exact shape of the
    // "inactive in this profile" case that reads as a broken card.
    IDeckLinkOutput* out_if = nullptr;
    if (device->QueryInterface(IID_IDeckLinkOutput,
                               reinterpret_cast<void**>(&out_if)) == S_OK) {
      d.has_output = 1;
      IDeckLinkDisplayModeIterator* modes = nullptr;
      if (out_if->GetDisplayModeIterator(&modes) == S_OK && modes != nullptr) {
        IDeckLinkDisplayMode* m = nullptr;
        if (modes->Next(&m) == S_OK && m != nullptr) {
          d.active = 1;
          safe_release(m);
        }
        safe_release(modes);
      }
      safe_release(out_if);
    }
    IDeckLinkInput* in_if = nullptr;
    if (device->QueryInterface(IID_IDeckLinkInput,
                               reinterpret_cast<void**>(&in_if)) == S_OK) {
      d.has_input = 1;
      IDeckLinkDisplayModeIterator* modes = nullptr;
      if (in_if->GetDisplayModeIterator(&modes) == S_OK && modes != nullptr) {
        IDeckLinkDisplayMode* m = nullptr;
        if (modes->Next(&m) == S_OK && m != nullptr) {
          d.active = 1;
          safe_release(m);
        }
        safe_release(modes);
      }
      safe_release(in_if);
    }

    if (inactive_by_duplex) d.active = 0;

    out[n++] = d;
    safe_release(device);
  }
  safe_release(it);
  return n;
}

void* kd_capture_open(int64_t persistent_id, kd_frame_fn cb, void* ctx) {
  auto* c = new Capture(cb, ctx);
  if (!c->open(persistent_id)) {
    c->Release();
    return nullptr;
  }
  return c;
}

void kd_capture_close(void* handle) {
  if (handle == nullptr) return;
  static_cast<Capture*>(handle)->Release();
}

void* kd_output_open(int64_t persistent_id, int32_t width, int32_t height,
                     int64_t rate_num, int64_t rate_den, int32_t interlaced) {
  auto* p = new Playback();
  if (!p->open(persistent_id, width, height, rate_num, rate_den,
               interlaced != 0)) {
    p->close();
    delete p;
    return nullptr;
  }
  return p;
}

int32_t kd_output_schedule(void* handle, const uint8_t* bytes,
                           int32_t row_bytes) {
  if (handle == nullptr || bytes == nullptr) return KD_ERR_FAILED;
  return static_cast<Playback*>(handle)->push(bytes, row_bytes);
}

int32_t kd_output_stats_get(void* handle, kd_output_stats* out) {
  if (handle == nullptr || out == nullptr) return KD_ERR_FAILED;
  static_cast<Playback*>(handle)->stats(out);
  return KD_OK;
}

void kd_output_close(void* handle) {
  if (handle == nullptr) return;
  auto* p = static_cast<Playback*>(handle);
  p->close();
  delete p;
}

}  // extern "C"
