#import <AVFoundation/AVFoundation.h>
#import <AudioToolbox/AudioToolbox.h>
#import <CoreAudio/CoreAudio.h>
#import <MediaToolbox/MediaToolbox.h>
#import <CommonCrypto/CommonDigest.h>
#include "atmos_assist.h"
#include <atomic>
#include <cstring>
#include <memory>
#include <mutex>
#include <string>

namespace {
std::atomic<uint32_t> live_sessions{0};
struct Metrics {
    std::atomic<uint32_t> state{0}, channels{0}, items{0}, taps{0}, device{0};
    std::atomic<uint64_t> generation{1}, frames{0}, loops{0}, tap_errors{0};
    std::atomic<bool> closed{false};
    std::atomic<uint32_t> desired{0};
    std::mutex error_mutex;
    std::string error;
};
static_assert(std::atomic<uint64_t>::is_always_lock_free);

struct TapContext {
    std::shared_ptr<Metrics> metrics;
    explicit TapContext(std::shared_ptr<Metrics> value) : metrics(std::move(value)) { metrics->taps++; }
    ~TapContext() { metrics->taps--; }
};

void tap_init(MTAudioProcessingTapRef, void* info, void** storage) { *storage = info; }
void tap_finalize(MTAudioProcessingTapRef tap) {
    delete static_cast<TapContext*>(MTAudioProcessingTapGetStorage(tap));
}
void tap_prepare(MTAudioProcessingTapRef tap, CMItemCount, const AudioStreamBasicDescription* asbd) {
    auto* context = static_cast<TapContext*>(MTAudioProcessingTapGetStorage(tap));
    context->metrics->channels.store(asbd->mChannelsPerFrame, std::memory_order_relaxed);
}
void tap_unprepare(MTAudioProcessingTapRef) {}
void tap_process(MTAudioProcessingTapRef tap, CMItemCount requested, MTAudioProcessingTapFlags,
                 AudioBufferList* buffers, CMItemCount* frames, MTAudioProcessingTapFlags* flags) {
    auto* context = static_cast<TapContext*>(MTAudioProcessingTapGetStorage(tap));
    const OSStatus status = MTAudioProcessingTapGetSourceAudio(tap, requested, buffers, flags, nullptr, frames);
    // Clear even the error path. The helper's data must never reach an audible output.
    for (UInt32 i = 0; i < buffers->mNumberBuffers; ++i) {
        auto& buffer = buffers->mBuffers[i];
        if (buffer.mData) std::memset(buffer.mData, 0, buffer.mDataByteSize);
    }
    if (status != noErr) {
        *frames = 0;
        context->metrics->tap_errors.fetch_add(1, std::memory_order_relaxed);
    } else if (*frames > 0) {
        context->metrics->frames.fetch_add(static_cast<uint64_t>(*frames), std::memory_order_relaxed);
    }
}

uint32_t default_device() {
    AudioDeviceID device = kAudioObjectUnknown;
    UInt32 size = sizeof(device);
    AudioObjectPropertyAddress property{kAudioHardwarePropertyDefaultOutputDevice,
        kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
    AudioObjectGetPropertyData(kAudioObjectSystemObject, &property, 0, nullptr, &size, &device);
    return device;
}
} // namespace

@interface MRAtmosItem : AVPlayerItem {
@public
    std::shared_ptr<Metrics> metrics_;
    uint64_t generation_;
}
@end
@implementation MRAtmosItem
- (void)dealloc { if (metrics_) metrics_->items--; }
@end

namespace {
struct Session : std::enable_shared_from_this<Session> {
    const std::shared_ptr<Metrics> metrics = std::make_shared<Metrics>();
    const uint32_t faults;
    NSData* data;
    AVURLAsset* asset = nil;
    AVAssetTrack* track = nil;
    AVQueuePlayer* player = nil;
    id end_observer = nil;
    id fail_observer = nil;
    dispatch_source_t health_timer = nil;
    AudioObjectPropertyListenerBlock route_listener = nil;
    bool preparing = false;

    Session(const uint8_t* bytes, size_t length, uint32_t flags) : faults(flags) {
        data = [NSData dataWithBytes:bytes length:length];
        metrics->device = default_device();
        live_sessions++;
    }
    ~Session() { live_sessions--; }

    bool current(uint64_t generation) const {
        return !metrics->closed.load() && metrics->generation.load() == generation;
    }

    void install_route_listener() {
        if (metrics->closed.load()) return;
        std::weak_ptr<Session> weak = shared_from_this();
        route_listener = ^(UInt32, const AudioObjectPropertyAddress*) {
            if (auto self = weak.lock()) self->metrics->device = default_device();
        };
        AudioObjectPropertyAddress property{kAudioHardwarePropertyDefaultOutputDevice,
            kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
        AudioObjectAddPropertyListenerBlock(kAudioObjectSystemObject, &property,
            dispatch_get_main_queue(), route_listener);
    }

    void clear_player() {
        preparing = false;
        if (health_timer) { dispatch_source_cancel(health_timer); health_timer = nil; }
        if (end_observer) [[NSNotificationCenter defaultCenter] removeObserver:end_observer];
        if (fail_observer) [[NSNotificationCenter defaultCenter] removeObserver:fail_observer];
        end_observer = nil;
        fail_observer = nil;
        [player pause];
        [player removeAllItems];
        player = nil;
        track = nil;
        asset = nil;
    }

    void close() {
        clear_player();
        if (route_listener) {
            AudioObjectPropertyAddress property{kAudioHardwarePropertyDefaultOutputDevice,
                kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
            AudioObjectRemovePropertyListenerBlock(kAudioObjectSystemObject, &property,
                dispatch_get_main_queue(), route_listener);
            route_listener = nil;
        }
    }

    void fail(NSString* message) {
        clear_player();
        { std::lock_guard lock(metrics->error_mutex); metrics->error = message.UTF8String ?: "Atmos helper failed"; }
        metrics->state = 4;
    }

    MRAtmosItem* make_item(uint64_t generation) {
        if (faults & 2) return nil;
        auto* context = new TapContext(metrics);
        MTAudioProcessingTapCallbacks callbacks{kMTAudioProcessingTapCallbacksVersion_0, context,
            tap_init, tap_finalize, tap_prepare, tap_unprepare, tap_process};
        MTAudioProcessingTapRef tap = nullptr;
        const OSStatus status = MTAudioProcessingTapCreate(kCFAllocatorDefault, &callbacks,
            kMTAudioProcessingTapCreationFlag_PostEffects, &tap);
        if (status != noErr || !tap) { if (tap) CFRelease(tap); else delete context; return nil; }
        MRAtmosItem* item = [[MRAtmosItem alloc] initWithAsset:asset];
        item->metrics_ = metrics;
        item->generation_ = generation;
        metrics->items++;
        item.allowedAudioSpatializationFormats = AVAudioSpatializationFormatMonoStereoAndMultichannel;
        AVMutableAudioMixInputParameters* input = [AVMutableAudioMixInputParameters audioMixInputParametersWithTrack:track];
        input.audioTapProcessor = tap;
        CFRelease(tap);
        AVMutableAudioMix* mix = [AVMutableAudioMix audioMix];
        mix.inputParameters = @[input];
        item.audioMix = mix;
        return item;
    }

    void ready(AVURLAsset* loaded_asset, AVAssetTrack* loaded_track, uint64_t generation) {
        if (!current(generation) || metrics->desired == 0) return;
        asset = loaded_asset;
        track = loaded_track;
        MRAtmosItem* first = make_item(generation);
        MRAtmosItem* second = first ? make_item(generation) : nil;
        if (!first || !second) { fail(@"Could not install the silent audio tap"); return; }
        if (!current(generation) || metrics->desired == 0) return;
        player = [AVQueuePlayer queuePlayerWithItems:@[first, second]];
        player.volume = 1.0F;
        player.actionAtItemEnd = AVPlayerActionAtItemEndAdvance;
        std::weak_ptr<Session> weak = shared_from_this();
        end_observer = [[NSNotificationCenter defaultCenter]
            addObserverForName:AVPlayerItemDidPlayToEndTimeNotification object:nil queue:[NSOperationQueue mainQueue]
            usingBlock:^(NSNotification* note) {
                auto self = weak.lock();
                if (!self || !self->current(generation) || ![note.object isKindOfClass:[MRAtmosItem class]]) return;
                MRAtmosItem* item = note.object;
                if (item->metrics_.get() != self->metrics.get() || item->generation_ != generation) return;
                [self->player removeItem:item];
                MRAtmosItem* next = self->make_item(generation);
                if (!next || ![self->player canInsertItem:next afterItem:nil]) {
                    self->fail(@"Could not replenish the silent JOC loop"); return;
                }
                [self->player insertItem:next afterItem:nil];
                self->metrics->loops++;
            }];
        fail_observer = [[NSNotificationCenter defaultCenter]
            addObserverForName:AVPlayerItemFailedToPlayToEndTimeNotification object:nil queue:[NSOperationQueue mainQueue]
            usingBlock:^(NSNotification* note) {
                auto self = weak.lock();
                if (!self || !self->current(generation) || ![note.object isKindOfClass:[MRAtmosItem class]]) return;
                MRAtmosItem* item = note.object;
                if (item->metrics_.get() == self->metrics.get()) self->fail(@"Silent JOC playback failed");
            }];
        health_timer = dispatch_source_create(DISPATCH_SOURCE_TYPE_TIMER, 0, 0, dispatch_get_main_queue());
        dispatch_source_set_timer(health_timer, DISPATCH_TIME_NOW, NSEC_PER_SEC / 4, NSEC_PER_SEC / 20);
        dispatch_source_set_event_handler(health_timer, ^{
            auto self = weak.lock();
            if (!self || !self->current(generation)) return;
            if (self->metrics->tap_errors.load() > 0 || self->player.status == AVPlayerStatusFailed ||
                self->player.currentItem.status == AVPlayerItemStatusFailed) {
                self->fail(self->player.error.localizedDescription ?: @"Silent JOC decoder/tap failed");
            } else if (self->metrics->desired == 2 && self->metrics->frames.load() > 0) {
                self->metrics->state = 2;
            }
        });
        dispatch_resume(health_timer);
        preparing = false;
        if (metrics->desired == 2) [player play];
        else metrics->state = 3;
    }

    void begin(uint64_t generation) {
        if (preparing || player || metrics->state == 4) return;
        preparing = true;
        metrics->state = 1;
        auto self = shared_from_this();
        dispatch_async(dispatch_get_global_queue(QOS_CLASS_UTILITY, 0), ^{
            @autoreleasepool {
                if (!self->current(generation)) return;
                NSError* error = nil;
                NSURL* url = self->cache_asset(&error);
                if (!url) {
                    NSString* message = error.localizedDescription ?: @"Could not read the embedded JOC asset";
                    dispatch_async(dispatch_get_main_queue(), ^{ if (self->current(generation)) self->fail(message); });
                    return;
                }
                AVURLAsset* loaded = [AVURLAsset URLAssetWithURL:url options:nil];
                [loaded loadTracksWithMediaType:AVMediaTypeAudio completionHandler:^(NSArray<AVAssetTrack*>* tracks, NSError* track_error) {
                    void (^finish)(void) = ^{
                        if (!self->current(generation)) return;
                        if (track_error || tracks.count == 0) self->fail(track_error.localizedDescription ?: @"Embedded JOC has no audio track");
                        else self->ready(loaded, tracks.firstObject, generation);
                    };
                    if (self->faults & 4) dispatch_after(dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC / 2), dispatch_get_main_queue(), finish);
                    else dispatch_async(dispatch_get_main_queue(), finish);
                }];
            }
        });
    }

    NSURL* cache_asset(NSError** error) {
        if ((faults & 1) || data.length == 0) return nil;
        unsigned char digest[CC_SHA256_DIGEST_LENGTH];
        CC_SHA256(data.bytes, static_cast<CC_LONG>(data.length), digest);
        NSMutableString* name = [NSMutableString stringWithCapacity:64];
        for (unsigned char byte : digest) [name appendFormat:@"%02x", byte];
        NSURL* directory = [[[NSFileManager defaultManager] URLsForDirectory:NSCachesDirectory inDomains:NSUserDomainMask].firstObject
            URLByAppendingPathComponent:@"com.macinrender.macindecode-ac4-player/atmos-assist" isDirectory:YES];
        if (!directory || ![[NSFileManager defaultManager] createDirectoryAtURL:directory withIntermediateDirectories:YES attributes:nil error:error]) return nil;
        NSURL* url = [directory URLByAppendingPathComponent:[name stringByAppendingString:@".m4a"]];
        NSData* existing = [NSData dataWithContentsOfURL:url options:0 error:nil];
        if (![existing isEqualToData:data] && ![data writeToURL:url options:NSDataWritingAtomic error:error]) return nil;
        return url;
    }

    void apply(uint32_t mode, uint64_t generation) {
        if (!current(generation) || (mode == 2 && metrics->desired != mode)) return;
        if (mode == 0) {
            clear_player();
            { std::lock_guard lock(metrics->error_mutex); metrics->error.clear(); }
            metrics->state = 0;
            metrics->frames = 0;
            metrics->tap_errors = 0;
        } else if (mode == 1) {
            // AVQueuePlayer may keep pulling silent tap data after pause(). The
            // helper has no user-visible timeline, so release its items instead.
            clear_player();
            metrics->frames = 0;
            if (metrics->state != 4) metrics->state = 3;
        } else if (metrics->state != 4) {
            if (player) { [player play]; metrics->state = metrics->frames > 0 ? 2 : 1; }
            else begin(generation);
        }
    }
};
using AtmosHandle = std::shared_ptr<Session>;
} // namespace

void* mr_atmos_create(const uint8_t* bytes, size_t length, uint32_t flags) {
    if ((!bytes && length) || length > 8 * 1024 * 1024) return nullptr;
    try {
        auto session = std::make_shared<Session>(bytes, length, flags);
        auto* handle = new AtmosHandle(session);
        dispatch_async(dispatch_get_main_queue(), ^{ session->install_route_listener(); });
        return handle;
    } catch (...) { return nullptr; }
}
void mr_atmos_set_mode(void* raw, uint32_t mode) {
    if (!raw || mode > 2) return;
    auto self = *static_cast<AtmosHandle*>(raw);
    const uint32_t previous = self->metrics->desired.exchange(mode);
    if (previous == mode) return;
    const uint64_t generation = mode != 2 ? ++self->metrics->generation : self->metrics->generation.load();
    dispatch_async(dispatch_get_main_queue(), ^{ self->apply(mode, generation); });
}
int mr_atmos_poll(void* raw, mr_atmos_snapshot* out) {
    if (!raw || !out || out->size != sizeof(*out)) return 0;
    auto self = *static_cast<AtmosHandle*>(raw);
    auto& m = *self->metrics;
    out->state = m.state; out->generation = m.generation; out->frames = m.frames;
    out->loops = m.loops; out->tap_errors = m.tap_errors; out->channels = m.channels;
    out->live_items = m.items; out->live_taps = m.taps; out->default_device = m.device;
    std::lock_guard lock(m.error_mutex);
    std::memset(out->error, 0, sizeof(out->error));
    std::strncpy(out->error, m.error.c_str(), sizeof(out->error) - 1);
    return 1;
}
void mr_atmos_destroy(void* raw) {
    if (!raw) return;
    auto* handle = static_cast<AtmosHandle*>(raw);
    auto self = *handle;
    self->metrics->closed = true;
    self->metrics->generation++;
    self->metrics->desired = 0;
    dispatch_async(dispatch_get_main_queue(), ^{ self->close(); });
    delete handle;
}
uint32_t mr_atmos_live_sessions(void) { return live_sessions.load(); }
