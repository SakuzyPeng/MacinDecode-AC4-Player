#include "adm/c_api.h"
#ifdef __APPLE__
#include "mr_headmotion.h"
#endif

int main(void) {
    if (adm_api_version_major() != 1 || adm_api_version_minor() < 36) return 1;
    adm_context_t* context = adm_create_context();
    if (!context) return 2;
    adm_destroy_context(context);
#ifdef __APPLE__
    /* Creation/destruction alone does not request motion permission. */
    mr_headmotion_t* motion = mr_headmotion_create();
    if (!motion) return 3;
    mr_headmotion_destroy(motion);
#endif
    return 0;
}
