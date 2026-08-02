// Keeps macOS from throttling a Kestrel that is not the frontmost window.
//
// This is not a nicety. macOS App Nap demotes a GUI application the moment its
// window is occluded or it stops being frontmost: the whole process is niced
// (`ps` shows STAT `SN`) and its timers are coalesced. Measured on an M4 Max,
// a Kestrel whose window was merely covered by another app fell from 50.2 fps
// to **6.7 fps** on its frame-path thread — a thread that does no UI work at
// all and exists precisely so the UI cannot affect it. The logs filled with
// "fell behind" and every SDI output would have been dropping frames while
// looking, from the operator's chair, like nothing was wrong.
//
// `beginActivityWithOptions:` with `NSActivityLatencyCritical` is the documented
// way to say "this process is doing something time-critical, leave it alone".
// The returned token must be held for as long as that is true, which here is
// the life of the process.

#import <Foundation/Foundation.h>

static id g_activity = nil;

void kestrel_begin_activity(void) {
  if (g_activity != nil) {
    return;
  }
  @autoreleasepool {
    NSActivityOptions options =
        NSActivityUserInitiated | NSActivityLatencyCritical;
    id token = [[NSProcessInfo processInfo]
        beginActivityWithOptions:options
                          reason:@"Kestrel is driving live SDI outputs"];
    // Deliberately retained and never released: the activity ends when the
    // process does. Compiled without ARC, so this is the explicit retain.
    g_activity = [token retain];
  }
}
