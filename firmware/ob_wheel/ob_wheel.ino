// OB Wheel — Patchwork firmware (step 2 of 2)
//
// Board: PCBCUPID Glyph S3 (ESP32-S3)
// Wiring:
//   Encoder CLK → A1 (GPIO1)
//   Encoder DT  → A2 (GPIO2)
//   Encoder SW  → SDA (GPIO4)
//   WS2812 LED strip DATA → A6 (GPIO6)
//
// IDE settings (needed for the branded USB descriptor Patchwork auto-discovers):
//   Tools → USB Mode: "USB-OTG (TinyUSB)"
//   Tools → USB CDC On Boot: "Disabled"   (we bring up USBSerial ourselves,
//                                          after setting the descriptor strings)
// Debug fallback: if the board won't enumerate in OTG mode, switch USB Mode to
// "Hardware CDC and JTAG" + CDC On Boot "Enabled" — the ARDUINO_USB_MODE guard
// below makes this same sketch run there (fixed "Espressif" identity; Patchwork
// then adopts it via the content-probe: it hears /sys/ready within ~1.5 s of
// opening the port, just not instantly).
//
// Protocol (line-based, 115200 is irrelevant over native USB):
//   → Patchwork:  /sys/ready encoder <ID>        boot + every 1 s (keep-alive;
//                                                Patchwork marks devices
//                                                inactive after 5 s of silence)
//                 /encoder/<ID>/turn <±1>        one line per detent
//                 /encoder/<ID>/click <0|1>      on switch change
//   ← Patchwork:  /encoder/<ID>/color <r> <g> <b>   strip colour (0..255)
//                 /encoder/<ID>/blink                identify blink
//                 ACK                                identification nudge →
//                                                    reply /sys/ready now
//
// Library: Adafruit NeoPixel (Library Manager).

#include <Adafruit_NeoPixel.h>

const uint8_t DEVICE_ID = 1;

// Raw GPIO numbers on purpose — the Arduino A1/A2/SDA aliases map to DIFFERENT
// GPIOs under generic ESP32-S3 board definitions (A1→2, A2→3, SDA→8), which is
// what made DT/SW read stuck-high in earlier tests. These match the Glyph S3
// silkscreen: A1=GPIO1, A2=GPIO2, SDA=GPIO4, A6=GPIO6.
const int PIN_CLK = 1;  // A1
const int PIN_DT  = 2;  // A2
const int PIN_SW  = 4;  // SDA
const int PIN_LED = 6;  // A6
const int NUM_LEDS = 8; // set to your strip length

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

// ── Encoder state (quadrature table decode, 4 sub-steps per detent) ─────────
const int8_t QUAD_TABLE[16] = {
   0, -1,  1,  0,
   1,  0,  0, -1,
  -1,  0,  0,  1,
   0,  1, -1,  0,
};
uint8_t prevState = 0;
int8_t  subSteps  = 0;

// ── Switch state ─────────────────────────────────────────────────────────────
bool     swState     = false;
bool     swRaw       = false;
uint32_t swChangedAt = 0;

// ── LED state ────────────────────────────────────────────────────────────────
uint8_t  ledColor[3] = {40, 40, 40};   // dim white until Patchwork assigns one
int      blinksLeft  = 0;              // pending blink half-cycles
uint32_t blinkNextAt = 0;
bool     blinkOn     = false;

uint32_t lastReadyAt = 0;
char     lineBuf[96];
size_t   lineLen = 0;

void showColor(uint8_t r, uint8_t g, uint8_t b) {
  for (int i = 0; i < NUM_LEDS; i++) strip.setPixelColor(i, strip.Color(r, g, b));
  strip.show();
}

void sendReady() {
  OBSerial.printf("/sys/ready encoder %u\n", DEVICE_ID);
}

void handleLine(const char *line) {
  if (strcmp(line, "ACK") == 0) { sendReady(); return; }

  // /encoder/<ID>/color r g b
  char prefix[32];
  snprintf(prefix, sizeof(prefix), "/encoder/%u/color", DEVICE_ID);
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

  // /encoder/<ID>/blink
  snprintf(prefix, sizeof(prefix), "/encoder/%u/blink", DEVICE_ID);
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
  USB.productName("OB Wheel");
  USB.serialNumber(sn);
  USBSerial.begin();
  USB.begin();
#else
  Serial.begin(115200);
#endif

  pinMode(PIN_CLK, INPUT_PULLUP);
  pinMode(PIN_DT,  INPUT_PULLUP);
  pinMode(PIN_SW,  INPUT_PULLUP);
  prevState = (digitalRead(PIN_CLK) << 1) | digitalRead(PIN_DT);

  strip.begin();
  showColor(ledColor[0], ledColor[1], ledColor[2]);

  sendReady();
  lastReadyAt = millis();
}

void loop() {
  uint32_t now = millis();

  // ── Encoder ──
  uint8_t state = (digitalRead(PIN_CLK) << 1) | digitalRead(PIN_DT);
  if (state != prevState) {
    subSteps += QUAD_TABLE[(prevState << 2) | state];
    prevState = state;
    if (subSteps >= 4 || subSteps <= -4) {
      int dir = (subSteps > 0) ? 1 : -1;
      subSteps = 0;
      OBSerial.printf("/encoder/%u/turn %d\n", DEVICE_ID, dir);
    }
  }

  // ── Switch (active low, 25 ms debounce) ──
  bool raw = (digitalRead(PIN_SW) == LOW);
  if (raw != swRaw) { swRaw = raw; swChangedAt = now; }
  if (swRaw != swState && now - swChangedAt > 25) {
    swState = swRaw;
    OBSerial.printf("/encoder/%u/click %d\n", DEVICE_ID, swState ? 1 : 0);
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
