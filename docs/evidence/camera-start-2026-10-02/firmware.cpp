#define CAM_SDA_PIN 1
#define CAM_SCL_PIN 2
#define CAM_XCLK_PIN 3
#define CAM_PCLK_PIN 4
#define CAM_VSYNC_PIN 5
#define CAM_HREF_PIN 6
#define CAM_D0_PIN 7
#define CAM_D1_PIN 8
#define CAM_D2_PIN 9
#define CAM_D3_PIN 10
#define CAM_D4_PIN 11
#define CAM_D5_PIN 12
#define CAM_D6_PIN 13
#define CAM_D7_PIN 14
#include <Arduino.h>
#include <esp_camera.h>

static void configureCamera(pixformat_t format) {
  camera_config_t config = {};
  config.pin_pwdn = -1;
  config.pin_reset = -1;
  config.pin_sccb_sda = CAM_SDA_PIN;
  config.pin_sccb_scl = CAM_SCL_PIN;
  config.pin_xclk = CAM_XCLK_PIN;
  config.pin_pclk = CAM_PCLK_PIN;
  config.pin_vsync = CAM_VSYNC_PIN;
  config.pin_href = CAM_HREF_PIN;
  config.pin_d0 = CAM_D0_PIN; config.pin_d1 = CAM_D1_PIN;
  config.pin_d2 = CAM_D2_PIN; config.pin_d3 = CAM_D3_PIN;
  config.pin_d4 = CAM_D4_PIN; config.pin_d5 = CAM_D5_PIN;
  config.pin_d6 = CAM_D6_PIN; config.pin_d7 = CAM_D7_PIN;
  config.xclk_freq_hz = 20000000;
  config.ledc_timer = LEDC_TIMER_0;
  config.ledc_channel = LEDC_CHANNEL_0;
  config.pixel_format = format;
  config.frame_size = format == PIXFORMAT_JPEG ? FRAMESIZE_QQVGA : FRAMESIZE_96X96;
  config.jpeg_quality = 12;
  config.fb_count = 1;
  config.fb_location = CAMERA_FB_IN_DRAM;
  config.grab_mode = CAMERA_GRAB_WHEN_EMPTY;
  esp_err_t result = esp_camera_init(&config);
  sensor_t *sensor = esp_camera_sensor_get();
  Serial.printf("CAMERA:READY:%d:%x\n", result, sensor ? sensor->id.PID : 0);
}

void setup() {
  Serial.begin(115200);
  configureCamera(PIXFORMAT_RGB565);
}

void loop() {
  if (!Serial.available()) { delay(1); return; }
  char command = Serial.read();
  if (command == 'C') {
    camera_fb_t *frame = esp_camera_fb_get();
    if (!frame) { Serial.println("CAMERA:EMPTY"); return; }
    uint32_t hash = 2166136261u;
    for (size_t i = 0; i < frame->len; i++) hash = (hash ^ frame->buf[i]) * 16777619u;
    Serial.printf("CAMERA:FRAME:%u:%u:%u:%u:%08lx:%02x:%02x\n", unsigned(frame->width), unsigned(frame->height), unsigned(frame->format), unsigned(frame->len), static_cast<unsigned long>(hash), frame->buf[0], frame->buf[frame->len - 1]);
    esp_camera_fb_return(frame);
  }
  if (command == 'Y') { esp_camera_deinit(); configureCamera(PIXFORMAT_YUV422); }
  if (command == 'G') { esp_camera_deinit(); configureCamera(PIXFORMAT_GRAYSCALE); }
  if (command == 'J') { esp_camera_deinit(); configureCamera(PIXFORMAT_JPEG); }
  if (command == 'B') ESP.restart();
}
