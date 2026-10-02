#include <Arduino.h>
#include <Wire.h>
#ifndef I2C_HEAVY
#include <SensirionI2cScd4x.h>
SensirionI2cScd4x sensor;
#endif

void setup() {
  Serial.begin(115200);
#ifdef I2C_HEAVY
  Wire.begin(8, 9, 100000);
  uint32_t sum = 0, errors = 0;
  for (unsigned i = 0; i < 5000; ++i) {
    Wire.beginTransmission(0x42);
    Wire.write(0);
    errors += Wire.endTransmission(false) != 0;
    if (Wire.requestFrom(uint8_t(0x42), uint8_t(1)) == 1) sum += Wire.read();
    else ++errors;
  }
  Serial.printf("HEAVY count=5000 sum=%lu errors=%lu\n", (unsigned long)sum, (unsigned long)errors);
#else
  Wire.begin(8, 9, 10000);
  sensor.begin(Wire, 0x62);
  Serial.printf("SCD stop %d\n", sensor.stopPeriodicMeasurement());
  delay(1);
  Serial.printf("SCD start %d\n", sensor.startPeriodicMeasurement());
  delay(5000);
  bool ready = false;
  int16_t error = 0;
  for (int attempt = 0; attempt < 8; ++attempt) {
    error = sensor.getDataReadyStatus(ready);
    if (!error) break;
    delayMicroseconds(1500);
  }
  Serial.printf("SCD ready %d %u\n", error, ready);
  uint16_t co2 = 0;
  float temperature = 0, humidity = 0;
  for (int attempt = 0; attempt < 8; ++attempt) {
    error = sensor.readMeasurement(co2, temperature, humidity);
    if (!error) break;
    delayMicroseconds(1500);
  }
  Serial.printf("SCD sample %d %u %.2f %.2f\n", error, co2, temperature, humidity);
#endif
  Serial.println("TIMING DONE");
}
void loop() { delay(1000); }
