// OB Pressure — Patchwork firmware (step 2 of 2)
//
// Board: PCBCUPID Glyph S3 (ESP32-S3)
// Wiring (FSR voltage divider):
//   FSR leg 1            → 3V3
//   FSR leg 2 = junction → A1 (GPIO1)   ADC input
//   10 kΩ                → junction ↔ GND   fixed pull-down
//   WS2812 LED strip DATA → A2 (GPIO2)
//
// IDE settings (needed for the branded USB descriptor Patchwork auto-discovers):
//   Tools → USB Mode: "USB-OTG (TinyUSB)"
//   Tools → USB CDC On Boot: "Disabled"
// Debug fallback: if the board won't enumerate in OTG mode, switch USB Mode to
// "Hardware CDC and JTAG" + CDC On Boot "Enabled" — the ARDUINO_USB_MODE guard
// below makes this same sketch run there (fixed "Espressif" identity; Patchwork
// then adopts it via the content-probe within ~1.5 s of opening the port).
//
// Protocol (line-based):
//   → Patchwork:  /sys/ready pressure <ID>       boot + every 1 s (keep-alive;
//                                                Patchwork marks devices
//                                                inactive after 5 s of silence)
//                 /pressure/<ID>/val <0..1>       on change past the deadband
//   ← Patchwork:  /pressure/<ID>/color <r> <g> <b>   strip colour (0..255)
//                 /pressure/<ID>/blink               identify blink
//                 ACK                                identification nudge →
//                                                    reply /sys/ready now
//
// Library: Adafruit NeoPixel (Library Manager).

#include <Adafruit_NeoPixel.h>

const uint8_t DEVICE_ID = 1;

// Raw GPIO numbers on purpose (see OB Wheel note): board aliases can map to
// different GPIOs under generic ESP32-S3 board defs. These match the Glyph S3
// silkscreen: A1=GPIO1, A2=GPIO2.
const int PIN_ADC = 1;  // A1, FSR junction
const int PIN_LED = 2;  // A2, WS2812 DATA
const int NUM_LEDS = 8; // set to your strip length

// FSR: no force → junction ~0 → val ~0; press → val rises. So NO inversion
// (unlike the knob pot). Send only when the value moves past DEADBAND, to
// avoid flooding the port with ADC jitter.
const float ADC_MAX  = 4095.0f;  // ESP32-S3 12-bit ADC
const float DEADBAND = 0.005f;   // ~0.5% of full scale

#if ARDUINO_USB_MODE
// Hardware CDC and JTAG mode (debug fallback) — identity is fixed "Espressif",
// Serial is the USB CDC (requires CDC On Boot: Enabled).
#define OBSerial Serial
#else
// USB-OTG (TinyUSB) — custom descriptor strings apply. Needs an explicit
// USBCDC object: branding without one enumerates but exposes no serial port.
#include "USB.h"
USBCDC USBSerial;
#define OBSerial USBSerial
#endif

Adafruit_NeoPixel strip(NUM_LEDS, PIN_LED, NEO_GRB + NEO_KHZ800);

// ── Sensor state ─────────────────────────────────────────────────────────────
float    lastSent    = -1.0f;   // force first send
uint32_t lastReadAt  = 0;

// ── LED state ────────────────────────────────────────────────────────────────
uint8_t  ledColor[3] = {40, 40, 40};   // dim white until Patchwork assigns one
int      blinksLeft  = 0;
uint32_t blinkNextAt = 0;
bool     blinkOn     = false;

uint32_t lastReadyAt = 0;
char     lineBuf[96];
size_t   lineLen = 0;

int median3(int a, int b, int c) {
  if (a > b) { int t = a; a = b; b = t; }
  if (b > c) { int t = b; b = c; c = t; }
  if (a > b) { int t = a; a = b; b = t; }
  return b;
}

void showColor(uint8_t r, uint8_t g, uint8_t b) {
  for (int i = 0; i < NUM_LEDS; i++) strip.setPixelColor(i, strip.Color(r, g, b));
  strip.show();
}

void sendReady() {
  OBSerial.printf("/sys/ready pressure %u\n", DEVICE_ID);
}

void handleLine(const char *line) {
  if (strcmp(line, "ACK") == 0) { sendReady(); return; }

  // /pressure/<ID>/color r g b
  char prefix[32];
  snprintf(prefix, sizeof(prefix), "/pressure/%u/color", DEVICE_ID);
  if (strncmp(line, prefix, strlen(prefix)) == 0) {
    int r, g, b;
    if (sscanf(line + strlen(prefix), "%d %d %d", &r, &g, &b) == 3) {
      ledColor[0] = constrain(r, 0, 255);
      ledColor[1] = constrain(g, 0, 255);
      ledColor[2] = constrain(b, 0, 255);
      if (blinksLeft == 0) showColor(ledColor[0], ledColor[1], ledColor[2]);
    }
    return;
  }

  // /pressure/<ID>/blink
  snprintf(prefix, sizeof(prefix), "/pressure/%u/blink", DEVICE_ID);
  if (strncmp(line, prefix, strlen(prefix)) == 0) {
    blinksLeft = 6;   // 3 on/off cycles
    blinkNextAt = 0;  // start immediately
    return;
  }
}

void setup() {
#if !ARDUINO_USB_MODE
  // Descriptor strings MUST be set before USB.begin() (with CDC On Boot
  // enabled USB starts before setup() and these calls are ignored — that's
  // the classic "still says Espressif" failure). Manufacturer "OpenBlocks"
  // is what Patchwork's auto-discovery keys on; the serial number is unused
  // for binding (the protocol carries the ID) but kept consistent with it.
  char sn[4];
  snprintf(sn, sizeof(sn), "%u", DEVICE_ID);
  USB.manufacturerName("OpenBlocks");
  USB.productName("OB Pressure");
  USB.serialNumber(sn);
  USBSerial.begin();
  USB.begin();
#else
  Serial.begin(115200);
#endif

  analogReadResolution(12);

  strip.begin();
  showColor(ledColor[0], ledColor[1], ledColor[2]);

  sendReady();
  lastReadyAt = millis();
}

void loop() {
  uint32_t now = millis();

  // ── Sensor (read ~50 Hz, send on change) ──
  if (now - lastReadAt >= 20) {
    lastReadAt = now;
    int raw = median3(analogRead(PIN_ADC), analogRead(PIN_ADC), analogRead(PIN_ADC));
    float val = raw / ADC_MAX;
    if (val < 0.0f) val = 0.0f;
    if (val > 1.0f) val = 1.0f;
    if (lastSent < 0.0f || fabsf(val - lastSent) > DEADBAND) {
      lastSent = val;
      OBSerial.printf("/pressure/%u/val %.3f\n", DEVICE_ID, val);
    }
  }

  // ── Keep-alive ──
  if (now - lastReadyAt >= 1000) {
    sendReady();
    lastReadyAt = now;
  }

  // ── Blink animation (non-blocking) ──
  if (blinksLeft > 0 && now >= blinkNextAt) {
    blinkOn = !blinkOn;
    if (blinkOn) showColor(255, 255, 255);
    else         showColor(0, 0, 0);
    blinkNextAt = now + 120;
    if (--blinksLeft == 0) showColor(ledColor[0], ledColor[1], ledColor[2]);
  }

  // ── Incoming commands ──
  while (OBSerial.available()) {
    char c = (char)OBSerial.read();
    if (c == '\n' || c == '\r') {
      if (lineLen > 0) {
        lineBuf[lineLen] = '\0';
        handleLine(lineBuf);
        lineLen = 0;
      }
    } else if (lineLen < sizeof(lineBuf) - 1) {
      lineBuf[lineLen++] = c;
    } else {
      lineLen = 0; // overflow — drop the line
    }
  }
}
