#include <stdint.h>
#include <stddef.h>
#include <string.h>

typedef struct {
  uint8_t *pointer;
  size_t length;
  size_t capacity;
} LensoBufferV1;

typedef struct {
  uint32_t abi_version;
  size_t struct_size;
  void *allocator_context;
  LensoBufferV1 (*allocate)(void *, size_t);
  LensoBufferV1 (*reallocate)(void *, LensoBufferV1, size_t);
  uint32_t (*free)(void *, LensoBufferV1);
  size_t max_result_bytes;
  size_t reserved[8];
} LensoHostV1;

typedef struct {
  uint32_t abi_version;
  size_t struct_size;
  void *plugin_context;
  const uint8_t *descriptor_json;
  size_t descriptor_json_len;
  uint32_t (*invoke)(void *, const uint8_t *, size_t, const uint8_t *, size_t,
                     const uint8_t *, size_t, LensoBufferV1 *);
  uint32_t (*shutdown)(void *);
  size_t reserved[8];
} LensoPluginV1;

static LensoHostV1 HOST;
static const char DESCRIPTOR[] =
  "{\"capabilities\":[{\"capability_id\":\"test.echo@1\","
  "\"descriptor_version\":\"1.0.0\","
  "\"request_operations\":[\"echo\",\"fail\"]}]}";

static uint32_t invoke(void *context, const uint8_t *capability, size_t capability_len,
                       const uint8_t *operation, size_t operation_len,
                       const uint8_t *request, size_t request_len,
                       LensoBufferV1 *output) {
  (void)context;
  (void)capability;
  (void)capability_len;
  const char *declared = "\"declared\"";
  const uint8_t *bytes = request;
  size_t length = request_len;
  uint32_t status = 0;
  if (operation_len == 4 && memcmp(operation, "fail", 4) == 0) {
    bytes = (const uint8_t *)declared;
    length = strlen(declared);
    status = 1;
  }
  *output = HOST.allocate(HOST.allocator_context, length);
  if (output->pointer == NULL || output->capacity < length) return 2;
  memcpy(output->pointer, bytes, length);
  output->length = length;
  return status;
}

static uint32_t shutdown_plugin(void *context) {
  (void)context;
  return 0;
}

#if defined(_WIN32)
__declspec(dllexport)
#else
__attribute__((visibility("default")))
#endif
uint32_t lenso_plugin_v1(const LensoHostV1 *host, LensoPluginV1 *plugin) {
  if (host == NULL || plugin == NULL || host->abi_version != 1 || host->allocate == NULL) return 2;
  HOST = *host;
  memset(plugin, 0, sizeof(*plugin));
  plugin->abi_version = 1;
  plugin->struct_size = sizeof(*plugin);
  plugin->descriptor_json = (const uint8_t *)DESCRIPTOR;
  plugin->descriptor_json_len = strlen(DESCRIPTOR);
  plugin->invoke = invoke;
  plugin->shutdown = shutdown_plugin;
  return 0;
}
