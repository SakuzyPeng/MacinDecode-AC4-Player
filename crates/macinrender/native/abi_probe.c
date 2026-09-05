#include <stddef.h>
#include <stdint.h>
#include "adm/c_api.h"
#include "mr_headmotion.h"

#if ADM_API_VERSION < 13600
#error MacinDecode requires MacinRender C ABI v1.36 or later
#endif

size_t macinrender_abi_size(uint32_t type) {
    switch (type) {
    case 0: return sizeof(adm_scene_renderer_config_t);
    case 1: return sizeof(adm_scene_stream_config_t);
    case 2: return sizeof(adm_scene_element_descriptor_t);
    case 3: return sizeof(adm_scene_object_state_t);
    case 4: return sizeof(adm_scene_pcm_plane_t);
    case 5: return sizeof(adm_scene_initial_state_t);
    case 6: return sizeof(adm_scene_metadata_update_t);
    case 7: return sizeof(adm_scene_frame_t);
    case 8: return sizeof(adm_scene_output_config_t);
    case 9: return sizeof(adm_scene_output_status_t);
    case 10: return sizeof(mr_headmotion_sample_t);
    default: return 0;
    }
}

size_t macinrender_abi_offset(uint32_t field) {
    switch (field) {
    case 0: return offsetof(adm_scene_renderer_config_t, speaker_geometry);
    case 1: return offsetof(adm_scene_stream_config_t, input_sample_rate);
    case 2: return offsetof(adm_scene_stream_config_t, input_queue_bytes);
    case 3: return offsetof(adm_scene_element_descriptor_t, semantic_identity);
    case 4: return offsetof(adm_scene_object_state_t, valid_fields);
    case 5: return offsetof(adm_scene_object_state_t, linear_gain);
    case 6: return offsetof(adm_scene_object_state_t, head_locked);
    case 7: return offsetof(adm_scene_pcm_plane_t, samples);
    case 8: return offsetof(adm_scene_pcm_plane_t, has_signal);
    case 9: return offsetof(adm_scene_initial_state_t, state);
    case 10: return offsetof(adm_scene_metadata_update_t, changed_fields);
    case 11: return offsetof(adm_scene_metadata_update_t, state);
    case 12: return offsetof(adm_scene_frame_t, media_sample_start);
    case 13: return offsetof(adm_scene_frame_t, pcm);
    case 14: return offsetof(adm_scene_frame_t, metadata_updates);
    case 15: return offsetof(adm_scene_output_config_t, speaker_geometry);
    case 16: return offsetof(adm_scene_output_status_t, presented_frames);
    case 17: return offsetof(adm_scene_output_status_t, clock_kind);
    case 18: return offsetof(mr_headmotion_sample_t, w);
    default: return (size_t)-1;
    }
}
