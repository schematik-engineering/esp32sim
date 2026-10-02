#include <Arduino.h>
#include <esp_cpu.h>
#include <soc/gpio_reg.h>

static portMUX_TYPE lock = portMUX_INITIALIZER_UNLOCKED;
static const uint8_t grb[] = {11, 7, 13, 19, 17, 23, 31, 29, 37};

void setup() {
    Serial0.begin(115200);
    pinMode(4, OUTPUT);
    digitalWrite(4, LOW);
    delay(10);
    const uint32_t mhz = getCpuFrequencyMhz();
    portENTER_CRITICAL(&lock);
    for (uint8_t value : grb) {
        for (uint8_t mask = 0x80; mask; mask >>= 1) {
            const uint32_t high = mhz * ((value & mask) ? 800 : 400) / 1000;
            REG_WRITE(GPIO_OUT_W1TS_REG, 1 << 4);
            uint32_t start = esp_cpu_get_cycle_count();
            while (uint32_t(esp_cpu_get_cycle_count() - start) < high) {}
            REG_WRITE(GPIO_OUT_W1TC_REG, 1 << 4);
            start = esp_cpu_get_cycle_count();
            while (uint32_t(esp_cpu_get_cycle_count() - start) < mhz * 600 / 1000) {}
        }
    }
    portEXIT_CRITICAL(&lock);
    delayMicroseconds(100);
    Serial0.println("WAVEFORM DONE");
}
void loop() { delay(1000); }
