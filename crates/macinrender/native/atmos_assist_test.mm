// Real AVFoundation lifecycle/fault checks. Build and run on macOS's main thread.
#import <Foundation/Foundation.h>
#include "atmos_assist.h"
#include <cstdio>
#include <cstdlib>
#include <functional>

static void pump(double seconds) {
    NSDate* until = [NSDate dateWithTimeIntervalSinceNow:seconds];
    while (until.timeIntervalSinceNow > 0) {
        [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.02]];
    }
}
static void require(bool success, const char* message) {
    if (!success) { std::fprintf(stderr, "FAIL: %s\n", message); std::exit(1); }
}
static mr_atmos_snapshot snapshot(void* handle) {
    mr_atmos_snapshot value{};
    value.size = sizeof(value);
    require(mr_atmos_poll(handle, &value) != 0, "private C ABI snapshot size/lifetime");
    return value;
}
static bool wait_for(const std::function<bool()>& check, double timeout = 8) {
    NSDate* until = [NSDate dateWithTimeIntervalSinceNow:timeout];
    do { pump(0.05); if (check()) return true; } while (until.timeIntervalSinceNow > 0);
    return false;
}
static void destroy(void* handle) {
    mr_atmos_destroy(handle);
    require(wait_for([] { return mr_atmos_live_sessions() == 0; }), "asynchronous teardown releases session");
}
int main(int argc, char** argv) {
    @autoreleasepool {
        require(argc >= 2, "usage: atmos-assist-test asset.m4a [loop-test-seconds]");
        NSData* data = [NSData dataWithContentsOfFile:[NSString stringWithUTF8String:argv[1]]];
        require(data.length > 0, "read bundled JOC asset");
        auto create = [&](uint32_t fault) { return mr_atmos_create(static_cast<const uint8_t*>(data.bytes), data.length, fault); };
        for (uint32_t fault : {1U, 2U}) {
            void* h = create(fault);
            require(h, "create fault session");
            mr_atmos_set_mode(h, 2);
            require(wait_for([&] { return snapshot(h).state == 4; }), "injected failure reported");
            auto s = snapshot(h);
            require(s.frames == 0 && s.live_items == 0 && s.live_taps == 0, "failed preparation never starts playback");
            destroy(h);
        }
        const uint8_t invalid[] = {1, 2, 3, 4};
        void* bad = mr_atmos_create(invalid, sizeof(invalid), 0);
        mr_atmos_set_mode(bad, 2);
        require(wait_for([&] { return snapshot(bad).state == 4; }), "invalid media fails without crashing");
        destroy(bad);

        void* cancelled = create(4);
        mr_atmos_set_mode(cancelled, 2);
        pump(0.1);
        mr_atmos_set_mode(cancelled, 0);
        pump(0.8);
        auto cancelled_state = snapshot(cancelled);
        require(cancelled_state.state == 0 && cancelled_state.frames == 0 && cancelled_state.live_items == 0,
            "stale preparation cannot resurrect stopped helper");
        mr_atmos_set_mode(cancelled, 2);
        require(wait_for([&] { return snapshot(cancelled).frames > 0; }), "restart after cancelled preparation");
        destroy(cancelled);

        void* h = create(0);
        mr_atmos_set_mode(h, 2);
        require(wait_for([&] { return snapshot(h).state == 2; }), "asset starts native JOC decoding");
        require(snapshot(h).channels == 12, "bundled JOC decodes to 12-channel PCM on this host");
        mr_atmos_set_mode(h, 1);
        pump(1.5);
        const auto paused_frames = snapshot(h).frames;
        pump(1);
        auto after_pause = snapshot(h);
        std::printf("pause before=%llu after=%llu state=%u errors=%llu message=%s\n", paused_frames, after_pause.frames,
            after_pause.state, after_pause.tap_errors, after_pause.error);
        require(snapshot(h).frames == paused_frames && snapshot(h).state == 3, "pause stops decoder progression");
        mr_atmos_set_mode(h, 2);
        require(wait_for([&] { return snapshot(h).frames > paused_frames; }), "resume advances decoder");
        const double seconds = argc > 2 ? std::atof(argv[2]) : 65;
        for (double elapsed = 0; elapsed < seconds; elapsed += 1) {
            pump(1);
            const auto s = snapshot(h);
            require(s.state == 2 && s.tap_errors == 0, "loop remains active without tap errors");
            require(s.live_items <= 3 && s.live_taps <= 3, "bounded loop item/tap ownership");
            if (static_cast<int>(elapsed) % 10 == 0) {
                std::printf("seconds=%.0f frames=%llu loops=%llu items=%u taps=%u\n", elapsed,
                    s.frames, s.loops, s.live_items, s.live_taps);
                std::fflush(stdout);
            }
        }
        require(snapshot(h).loops >= static_cast<uint64_t>(seconds / 31), "loop boundary callbacks advance");
        mr_atmos_set_mode(h, 0);
        require(wait_for([&] { auto s = snapshot(h); return s.live_items == 0 && s.live_taps == 0; }), "stop releases all items/taps");
        for (int i = 0; i < 30; ++i) {
            mr_atmos_set_mode(h, 2);
            require(wait_for([&] { return snapshot(h).frames > 0; }), "repeated start");
            mr_atmos_set_mode(h, 0);
            require(wait_for([&] { auto s = snapshot(h); return s.state == 0 && s.live_items == 0 && s.live_taps == 0; }), "repeated stop returns to baseline");
        }
        destroy(h);
        std::puts("PASS: faults, cancellation, pause/resume, looping, 30 restarts and teardown");
    }
}
