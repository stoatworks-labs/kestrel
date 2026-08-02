//! Keeping the operating system out of the frame path's way.

/// Tell macOS this process is doing something time-critical.
///
/// Call once at startup, before the frame path starts. Without it, App Nap
/// demotes the whole process as soon as the window is not frontmost — measured
/// here as **50.2 fps falling to 6.7 fps** on the frame-path thread, with the
/// process showing as niced in `ps`. See `native/activity.m`.
///
/// A no-op everywhere else.
pub fn keep_awake() {
    #[cfg(target_os = "macos")]
    unsafe {
        kestrel_begin_activity();
    }
}

#[cfg(target_os = "macos")]
extern "C" {
    fn kestrel_begin_activity();
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
}

/// `QOS_CLASS_USER_INTERACTIVE` from `<pthread/qos.h>`.
#[cfg(target_os = "macos")]
const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;

/// Mark the calling thread as latency-critical.
///
/// Belt and braces alongside [`keep_awake`]: the process-wide activity stops
/// macOS demoting the *application*, and this stops the scheduler treating this
/// particular thread as background work when the app is not in front. The frame
/// path calls it on itself as its first act.
pub fn this_thread_is_realtime() {
    #[cfg(target_os = "macos")]
    unsafe {
        let rc = pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0);
        if rc != 0 {
            tracing::warn!(rc, "could not raise the frame thread's QoS");
        }
    }
}
