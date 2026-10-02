#include <Arduino.h>
#include <Wire.h>
#include <Adafruit_PWMServoDriver.h>

Adafruit_PWMServoDriver first(0x40), second(0x41), group(0x70);

void writeRegister(uint8_t address, uint8_t reg, uint8_t value) {
  Wire.beginTransmission(address);
  Wire.write(reg);
  Wire.write(value);
  Wire.endTransmission();
}

void setup() {
  Serial.begin(115200);
  Wire.begin(8, 9);
  // Initialize the group handle before enabling all-call on the individual devices.
  bool g = group.begin();
  bool a = first.begin(), b = second.begin();
  first.setPWMFreq(50);
  second.setPWMFreq(50);
  writeRegister(0x40, 0, 0x21);
  writeRegister(0x41, 0, 0x21);
  first.setPWM(0, 0, 300);
  second.setPWM(0, 0, 450);
  Serial.printf("PCA begin %u %u %u\n", a, b, g);
  Serial.printf("PCA main %u %u and %u\n", first.getPWM(0, true), second.getPWM(0, true), group.getPWM(0, true));
  group.setPWM(0, 10, 600);
  Serial.printf("PCA group %u %u\n", first.getPWM(0, true), second.getPWM(0, true));
  writeRegister(0x40, 0, 0x20);
  group.setPWM(0, 20, 700);
  Serial.printf("PCA disabled %u %u\n", first.getPWM(0, true), second.getPWM(0, true));
  Wire.beginTransmission(0);
  Wire.write(6);
  Serial.printf("PCA reset %u\n", Wire.endTransmission());
  Serial.println("PCA DONE");
}
void loop() { delay(1000); }
