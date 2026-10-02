#include <Arduino.h>
#include <esp_arduino_version.h>
#if CONFIG_IDF_TARGET_ESP32S3
constexpr int pin = 1;
#else
constexpr int pin = 0;
#endif
uint16_t samples[1024];
uint32_t times[1024];
void setup() {
  Serial.begin(115200);
  analogReadResolution(12);
  analogSetPinAttenuation(pin, ADC_11db);
  Serial.printf("ARDUINO=%s ADC READY\n", ESP_ARDUINO_VERSION_STR);
}
void loop() {
  if (!Serial.available()) { delay(1); return; }
  Serial.read();
  uint32_t next = micros();
  for (int i = 0; i < 1024; ++i) {
    while (int32_t(micros() - next) < 0) {}
    times[i] = micros();
    samples[i] = analogRead(pin);
    next += 125;
  }
  for (int i = 0; i < 1024; ++i)
    Serial.printf("SAMPLE %lu %u\n", (unsigned long)times[i], samples[i]);
  Serial.println("ADC DONE");
}
