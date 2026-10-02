#include <Arduino.h>
#include <ESP_I2S.h>
#include <esp_arduino_version.h>
#include <math.h>

I2SClass mic;
RTC_DATA_ATTR unsigned boots = 0;
void setup() {
  Serial0.begin(115200);
  delay(100);
  Serial0.printf("ARDUINO=%s BOOT=%u\n", ESP_ARDUINO_VERSION_STR, ++boots);
#ifdef TEST_PDM
  mic.setPinsPdmRx(4, 6);
  bool ok = mic.begin(I2S_MODE_PDM_RX, 16000, I2S_DATA_BIT_WIDTH_16BIT, I2S_SLOT_MODE_MONO);
#else
  mic.setPins(4, 5, -1, 6);
  bool ok = mic.begin(I2S_MODE_STD, 16000, I2S_DATA_BIT_WIDTH_16BIT, I2S_SLOT_MODE_MONO);
#endif
  Serial0.printf("RX_BEGIN=%d\n", ok);
  if (!ok) return;
  int16_t samples[256];
  size_t n = mic.readBytes((char *)samples, sizeof(samples));
  double energy = 0, peak = 0;
  int best = 0;
  for (int j = 0; j < 256; ++j) energy += (double)samples[j] * samples[j];
  for (int k = 1; k < 128; ++k) {
    double re = 0, im = 0;
    double c = cos(2 * M_PI * k / 256), s = sin(2 * M_PI * k / 256);
    double x = 1, y = 0;
    for (int j = 0; j < 256; ++j) {
      re += samples[j] * x; im += samples[j] * y;
      double next = x * c - y * s; y = y * c + x * s; x = next;
    }
    double power = re * re + im * im;
    if (power > peak) { peak = power; best = k; }
  }
  Serial0.printf("RX_BYTES=%u RMS=%.2f FREQ=%.2f\n", (unsigned)n, sqrt(energy / 256), best * 16000.0 / 256);
  mic.end();
  delay(100);
  if (boots < 2) ESP.restart();
}
void loop() { delay(1000); }
