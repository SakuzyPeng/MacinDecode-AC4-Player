// Real AVFoundation lifecycle/fault checks. Build and run on macOS's main thread.
#import <Foundation/Foundation.h>
#import <AVFoundation/AVFoundation.h>
#include "atmos_assist.h"
#include <cstdio>
#include <cstdlib>
#include <cstring>
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

static void verify_long_asset_tail(NSString* path) {
    AVURLAsset* asset = [AVURLAsset URLAssetWithURL:[NSURL fileURLWithPath:path] options:nil];
    __block AVAssetTrack* track = nil;
    __block bool loaded = false;
    [asset loadTracksWithMediaType:AVMediaTypeAudio completionHandler:^(NSArray<AVAssetTrack*>* tracks, NSError* error) {
        dispatch_async(dispatch_get_main_queue(), ^{
            track = error ? nil : tracks.firstObject;
            loaded = true;
        });
    }];
    NSDate* deadline = [NSDate dateWithTimeIntervalSinceNow:8];
    while (!loaded && deadline.timeIntervalSinceNow > 0) pump(0.05);
    require(loaded && track, "load continuous JOC track with AVFoundation");
    NSError* error = nil;
    AVAssetReader* reader = [[AVAssetReader alloc] initWithAsset:asset error:&error];
    require(reader && !error, "create native reader for 24-hour tail");
    constexpr int64_t end_frame = 4149411840LL;
    constexpr int64_t tail_frames = 3072;
    reader.timeRange = CMTimeRangeMake(CMTimeMake(end_frame - tail_frames, 48000), CMTimeMake(tail_frames, 48000));
    AVAssetReaderTrackOutput* output = [AVAssetReaderTrackOutput assetReaderTrackOutputWithTrack:track outputSettings:nil];
    require([reader canAddOutput:output], "native reader accepts compressed JOC track");
    [reader addOutput:output];
    require([reader startReading], "seek to the final repeated chunk in AVFoundation");
    int64_t next_frame = end_frame - tail_frames;
    bool first = true;
    while (CMSampleBufferRef buffer = [output copyNextSampleBuffer]) {
        // AVAssetReader also returns an empty end marker with an invalid PTS.
        // It carries no packet data and must not advance the media timeline.
        if (CMSampleBufferGetNumSamples(buffer) == 0) {
            require(CMSampleBufferGetTotalSampleSize(buffer) == 0, "native end marker contains no packet bytes");
            CFRelease(buffer);
            continue;
        }
        const CMTime pts = CMTimeConvertScale(CMSampleBufferGetPresentationTimeStamp(buffer), 48000, kCMTimeRoundingMethod_Default);
        const CMTime duration = CMTimeConvertScale(CMSampleBufferGetDuration(buffer), 48000, kCMTimeRoundingMethod_Default);
        if (!CMTIME_IS_NUMERIC(pts) || !CMTIME_IS_NUMERIC(duration) || duration.value <= 0) {
            std::fprintf(stderr, "tail buffer samples=%ld bytes=%zu pts=%lld/%d flags=%u duration=%lld/%d flags=%u\n",
                CMSampleBufferGetNumSamples(buffer), CMSampleBufferGetTotalSampleSize(buffer),
                pts.value, pts.timescale, pts.flags, duration.value, duration.timescale, duration.flags);
        }
        require(CMTIME_IS_NUMERIC(pts) && CMTIME_IS_NUMERIC(duration) && duration.value > 0,
            "native tail timestamps and duration are valid");
        if (first) {
            // Compressed passthrough may include one E-AC-3 packet of decoder
            // preroll before the requested range. Its timeline must still end
            // exactly at the asset boundary, without wrapping the uint32 mdhd.
            require(pts.value <= next_frame && pts.value >= next_frame - 1536,
                "native tail seek includes at most one compressed preroll packet");
            next_frame = pts.value;
            first = false;
        }
        if (pts.value != next_frame || duration.value > end_frame - next_frame) {
            std::fprintf(stderr, "tail pts=%lld duration=%lld expected=%lld end=%lld\n",
                pts.value, duration.value, next_frame, end_frame);
        }
        require(pts.value == next_frame && duration.value <= end_frame - next_frame,
            "native tail packets are contiguous within the 24-hour timeline");
        next_frame += duration.value;
        CFRelease(buffer);
    }
    require(reader.status == AVAssetReaderStatusCompleted && next_frame == end_frame,
        "AVFoundation reaches the exact continuous asset boundary");
}

int main(int argc, char** argv) {
    @autoreleasepool {
        require(argc >= 2, "usage: atmos-assist-test asset.m4a [continuous-test-seconds]");
        NSData* data = [NSData dataWithContentsOfFile:[NSString stringWithUTF8String:argv[1]]];
        require(data.length > 0, "read bundled JOC asset");
        verify_long_asset_tail([NSString stringWithUTF8String:argv[1]]);
        if (argc > 2 && std::strcmp(argv[2], "--asset-only") == 0) {
            std::puts("PASS: AVFoundation demuxes the exact 24-hour JOC tail");
            return 0;
        }
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
        require(seconds >= 1 && seconds <= 3600, "continuous regression duration must be 1..3600 seconds");
        auto previous_frames = snapshot(h).frames;
        for (double elapsed = 0; elapsed < seconds; elapsed += 1) {
            pump(1);
            const auto s = snapshot(h);
            require(s.state == 2 && s.tap_errors == 0, "continuous helper remains active without tap errors");
            require(s.frames > previous_frames, "JOC decoding progresses across former 30-second item boundaries");
            require(s.channels == 12, "continuous JOC retains the 12-channel format");
            require(s.loops == 0, "no player-item transition at a repeated packet boundary");
            require(s.live_items == 2 && s.live_taps == 2, "the same two long-lived items/taps remain resident");
            previous_frames = s.frames;
            if (static_cast<int>(elapsed) % 10 == 0) {
                std::printf("seconds=%.0f frames=%llu loops=%llu items=%u taps=%u\n", elapsed,
                    s.frames, s.loops, s.live_items, s.live_taps);
                std::fflush(stdout);
            }
        }
        mr_atmos_set_mode(h, 0);
        require(wait_for([&] { auto s = snapshot(h); return s.live_items == 0 && s.live_taps == 0; }), "stop releases all items/taps");
        for (int i = 0; i < 30; ++i) {
            mr_atmos_set_mode(h, 2);
            require(wait_for([&] { return snapshot(h).frames > 0; }), "repeated start");
            mr_atmos_set_mode(h, 0);
            require(wait_for([&] { auto s = snapshot(h); return s.state == 0 && s.live_items == 0 && s.live_taps == 0; }), "repeated stop returns to baseline");
        }
        destroy(h);

        // Production items last 24 hours. Exercise their eventual replenishment
        // independently with one-second end times, without shortening the asset
        // or reintroducing frequent transitions into real playback.
        void* short_items = create(8);
        mr_atmos_set_mode(short_items, 2);
        require(wait_for([&] {
            const auto s = snapshot(short_items);
            require(s.state != 4 && s.tap_errors == 0, "short item rollover remains healthy");
            require(s.live_items <= 3 && s.live_taps <= 3, "rollover keeps bounded item/tap ownership");
            return s.loops >= 5;
        }, 15), "eventual long-item rollover still replenishes the queue");
        destroy(short_items);
        std::puts("PASS: continuous JOC, faults, cancellation, pause/resume, rollover, 30 restarts and teardown");
    }
}
