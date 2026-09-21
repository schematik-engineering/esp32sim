#include <Arduino.h>
#include <WiFi.h>

void setup() {
  Serial0.begin(115200);
  Serial0.println("WIFI:BOOT");
  if (!WiFi.mode(WIFI_STA)) {
    Serial0.println("WIFI:MODE_FAILED");
    return;
  }
  Serial0.println("WIFI:SCAN_START");
  const int count = WiFi.scanNetworks();
  Serial0.printf("WIFI:SCAN_COUNT:%d\n", count);
  bool found = false;
  for (int i = 0; i < count; ++i) {
    if (WiFi.SSID(i) == "esp32sim") found = true;
  }
  Serial0.println(found ? "WIFI:AP_FOUND" : "WIFI:AP_MISSING");
  WiFi.scanDelete();
  WiFi.begin("esp32sim", "esp32sim-pass");
  const unsigned long deadline = millis() + 20000;
  while (WiFi.status() != WL_CONNECTED && millis() < deadline) delay(10);
  if (WiFi.status() == WL_CONNECTED) {
    Serial0.print("WIFI:IP:");
    Serial0.println(WiFi.localIP());
  } else {
    Serial0.printf("WIFI:CONNECT_FAILED:%d\n", WiFi.status());
  }
}
void loop() { delay(100); }
