// OB Pressure — serial test sketch (step 1 of 2)
//
// Board: PCBCUPID Glyph S3 (ESP32-S3)
// Wiring (FSR voltage divider):
//   FSR leg 1        → 3V3
//   FSR leg 2 = junction → A1 (GPIO1)   ADC input
//   10 kΩ            → junction ↔ GND    fixed pull-down
//   (LED strip on A2/GPIO2 — unused in this test)
//
// How it reads: no force → FSR resistance is huge → junction pulled to ~0 by the
// 10 kΩ → value ~0. Press → FSR resistance drops → junction voltage rises →
// value rises toward 1. (More force = higher value.)
//
// IDE: Tools → USB CDC On Boot: Enabled. Open the serial monitor at 115200.
// Prints raw ADC + normalized 0..1 a few times a second; press to watch it rise.

const int PIN_ADC = 1;  // A1, junction node

// ESP32-S3 ADC is 12-bit (0..4095).
const float ADC_MAX = 4095.0f;

// Median-of-3 to knock out single-sample spikes.
int median3(int a, int b, int c) {
  if (a > b) { int t = a; a = b; b = t; }
  if (b > c) { int t = b; b = c; c = t; }
  if (a > b) { int t = a; a = b; b = t; }
  return b;
}

void setup() {
  Serial.begin(115200);
  analogReadResolution(12);
  Serial.println("OB Pressure test — press the sensor");
}

void loop() {
  int raw = median3(analogRead(PIN_ADC), analogRead(PIN_ADC), analogRead(PIN_ADC));
  float norm = raw / ADC_MAX;
  Serial.printf("raw %4d  val %.3f\n", raw, norm);
  delay(150);
}
