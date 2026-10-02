#include <Arduino.h>
#include <SPI.h>

void setup() {
  Serial.begin(115200);
  pinMode(6, OUTPUT);
  pinMode(2, INPUT);
  pinMode(10, OUTPUT);
  pinMode(3, OUTPUT);
  digitalWrite(10, HIGH);
  digitalWrite(3, HIGH);
  digitalWrite(6, LOW);
  digitalWrite(10, LOW);
  uint32_t value = 0;
  for (unsigned bit = 0; bit < 32; ++bit) {
    digitalWrite(6, HIGH);
    value = (value << 1) | digitalRead(2);
    digitalWrite(6, LOW);
  }
  digitalWrite(10, HIGH);
  Serial.printf("SOFT=%08lx\n", (unsigned long)value);
  SPI.begin(6, 2, 7, -1);
  SPI.beginTransaction(SPISettings(1000000, MSBFIRST, SPI_MODE0));
  digitalWrite(10, LOW);
  uint8_t right = SPI.transfer(0x5a);
  digitalWrite(10, HIGH);
  digitalWrite(3, LOW);
  uint8_t wrong = SPI.transfer(0x5a);
  digitalWrite(3, HIGH);
  uint8_t released = SPI.transfer(0x5a);
  SPI.endTransaction();
  Serial.printf("SPI right=%02x wrong=%02x released=%02x\n", right, wrong, released);
}
void loop() { delay(1000); }
