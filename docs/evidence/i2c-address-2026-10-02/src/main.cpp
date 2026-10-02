#include <Arduino.h>
#include <Wire.h>

int identity(uint8_t address) {
  Wire.beginTransmission(address);
  Wire.write(0x01);
  Wire.write(0x0f);
  if (Wire.endTransmission(false) != 0) return -1;
  if (Wire.requestFrom(address, uint8_t(1)) != 1) return -2;
  return Wire.read();
}

void setup() {
  Serial.begin(115200);
  Wire.begin(8, 9);
  Serial.printf("I2C before %d\n", identity(0x29));
  Wire.beginTransmission(0x29);
  Wire.write(0x00);
  Wire.write(0x01);
  Wire.write(0x30);
  Serial.printf("I2C change %d\n", Wire.endTransmission());
  Wire.beginTransmission(0x29);
  Serial.printf("I2C old %d\n", Wire.endTransmission());
  Serial.printf("I2C after %d\n", identity(0x30));
  Wire.beginTransmission(0x00);
  Wire.write(0x06);
  Serial.printf("I2C reset %d\n", Wire.endTransmission());
  Serial.println("I2C DONE");
}
void loop() { delay(1000); }
