#pragma once
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif
// Player-private ABI. This is not part of the upstream MacinRender C API.
typedef struct {
    uint32_t size;
    uint32_t state; // 0 idle, 1 starting, 2 active, 3 paused, 4 failed
    uint64_t generation;
    uint64_t frames;
    uint64_t loops;
    uint64_t tap_errors;
    uint32_t channels;
    uint32_t live_items;
    uint32_t live_taps;
    uint32_t default_device;
    char error[384];
} mr_atmos_snapshot;

// bytes are copied before returning. flags are fault injection for native tests:
// 1 = cache read/write failure, 2 = tap creation failure, 4 = delayed preparation.
void* mr_atmos_create(const uint8_t* bytes, size_t length, uint32_t flags);
void mr_atmos_set_mode(void* handle, uint32_t mode); // 0 stop/release, 1 pause, 2 play
int mr_atmos_poll(void* handle, mr_atmos_snapshot* result);
void mr_atmos_destroy(void* handle); // never waits for the main queue
uint32_t mr_atmos_live_sessions(void);
#ifdef __cplusplus
}
#endif
