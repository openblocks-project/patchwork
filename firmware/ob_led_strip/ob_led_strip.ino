// OB LED Strip — Patchwork firmware (WS2812B output device)
//
// Board: PCBCUPID Glyph S3 (ESP32-S3)
// Wiring:
//   WS2812B DATA → A2 (GPIO2)
//   Strip 5V     → external 5V supply (NOT the board 3V3 for a long strip —
//                  100 LEDs at full white can pull several amps)
//   Strip GND    → supply GND AND board GND (common ground required)
//
// A 300–500 Ω resistor in series on DATA and a 1000 µF cap across the strip's
// 5V/GND are recommended for reliable WS2812B signalling.
//
// IDE settings (needed for the branded USB descriptor Patchwork auto-discovers):
//   Tools → USB Mode: "USB-OTG (TinyUSB)"
//   Tools → USB CDC On Boot: "Disabled"
// Debug fallback: "Hardware CDC and JTAG" + CDC On Boot "Enabled" — the
// ARDUINO_USB_MODE guard makes this same sketch run there (fixed "Espressif"
// identity; Patchwork adopts it via the content-probe instead of the fast path).
//
// Protocol (line-based):
//   → Patchwork:  /sys/ready light <ID>          boot + every 1 s (keep-alive;
//                                                Patchwork marks the device
//                                                inactive after 5 s of silence,
//                                                and routes commands only to a
//                                                port that has announced it)
//   ← Patchwork:  /light/<ID>/num N              strip length (LEDs)
//                 /light/<ID>/color R G B        base colour, 0..255
//                 /light/<ID>/effect M P B       M=mode(0 Solid / 1 Pointer /
//                                                2 Pulse / 3 Rainbow / 4 Chase /
//                                                5 Sparkle)
//                                                P=param 0..1 (pos | speed)
//                                                B=brightness 0..1
//                 ACK                            identification nudge → /sys/ready
//
// Library: Adafruit NeoPixel (Library Manager).

#include <Adafruit_NeoPixel.h>

const uint8_t DEVICE_ID = 1;

// Raw GPIO number (see OB Wheel note): Glyph S3 silkscreen A2 = GPIO2.
const int PIN_LED  = 2;    // A2, WS2812B DATA
const int MAX_LEDS = 300;  // allocation ceiling; runtime count set via /num

#if ARDUINO_USB_MODE
// Hardware CDC and JTAG mode (debug fallback) — fixed "Espressif" identity,
// Serial is the USB CDC (requires CDC On Boot: Enabled).
#define OBSerial Serial
#else
// USB-OTG (TinyUSB) — custom descriptor strings apply. Needs an explicit
// USBCDC object: branding without one enumerates but exposes no serial port.
#include "USB.h"
USBCDC USBSerial;
#define OBSerial USBSerial
#endif

Adafruit_NeoPixel strip(MAX_LEDS, PIN_LED, NEO_GRB + NEO_KHZ800);

// ── State pushed from Patchwork ──────────────────────────────────────────────
int      numLeds    = 100;    // /num
uint8_t  baseR = 255, baseG = 180, baseB = 100;  // /color
uint8_t  mode       = 0;      // /effect M
float    param      = 0.0f;   // /effect P (position | speed)
float    brightness = 0.20f;  // /effect B

uint32_t lastReadyAt = 0;
uint32_t lastFrameAt = 0;
char     lineBuf[96];
size_t   lineLen = 0;

// Per-pixel fade buffer for Sparkle (0..255 intensity, decays each frame).
uint8_t  sparkleLevel[MAX_LEDS] = {0};

// ── Rendering ────────────────────────────────────────────────────────────────
// Called every frame; `phase` (0..1) drives the Pulse breathe.
void renderStrip(float phase) {
  int n = numLeds;
  if (n < 1) n = 1;
  if (n > MAX_LEDS) n = MAX_LEDS;

  // Scale base colour by brightness (and, for Pulse, by the breathe envelope).
  float bri = brightness;
  if (bri < 0.0f) bri = 0.0f;
  if (bri > 1.0f) bri = 1.0f;

  if (mode == 1) {
    // Pointer: single lit LED (plus soft neighbours) at param position.
    for (int i = 0; i < n; i++) strip.setPixelColor(i, 0);
    int center = (int)(param * (n - 1) + 0.5f);
    for (int d = -1; d <= 1; d++) {
      int i = center + d;
      if (i < 0 || i >= n) continue;
      float falloff = (d == 0) ? 1.0f : 0.35f;
      float b = bri * falloff;
      strip.setPixelColor(i, strip.Color(baseR * b, baseG * b, baseB * b));
    }
  } else if (mode == 2) {
    // Pulse: whole strip breathes. `param` sets the rate; phase is a 0..1
    // triangle. Envelope 0.15..1.0 so it never fully blacks out.
    float env = 0.15f + 0.85f * (0.5f - 0.5f * cosf(phase * 2.0f * PI));
    float b = bri * env;
    for (int i = 0; i < n; i++)
      strip.setPixelColor(i, strip.Color(baseR * b, baseG * b, baseB * b));
  } else if (mode == 3) {
    // Rainbow: full-spectrum hue sweep across the strip, scrolling with phase.
    // Ignores base colour (the whole point is the spectrum). `param` = speed.
    uint8_t val = (uint8_t)(bri * 255.0f);
    for (int i = 0; i < n; i++) {
      float h = (float)i / (float)n + phase;
      h -= floorf(h);
      uint16_t hue = (uint16_t)(h * 65535.0f);
      strip.setPixelColor(i, strip.gamma32(strip.ColorHSV(hue, 255, val)));
    }
  } else if (mode == 4) {
    // Chase: a comet (bright head + fading tail) runs along the strip in the
    // base colour. `param` = run speed. Tail wraps around the ends.
    for (int i = 0; i < n; i++) strip.setPixelColor(i, 0);
    float headF = phase * (float)n;
    float tailLen = n * 0.25f;
    if (tailLen < 3.0f) tailLen = 3.0f;
    for (int i = 0; i < n; i++) {
      float d = headF - (float)i;
      while (d < 0.0f) d += (float)n;
      if (d <= tailLen) {
        float b = (1.0f - d / tailLen) * bri;
        strip.setPixelColor(i, strip.Color(baseR * b, baseG * b, baseB * b));
      }
    }
  } else if (mode == 5) {
    // Sparkle: random pixels flare to the base colour then fade out. `param`
    // sets how densely new sparkles ignite. Uses the persistent fade buffer.
    for (int i = 0; i < n; i++)
      sparkleLevel[i] = (uint8_t)(sparkleLevel[i] * 0.86f);
    int spawns = 1 + (int)(param * 4.0f);
    for (int s = 0; s < spawns; s++) {
      if (random(1000) < (int)((0.15f + param * 0.55f) * 1000.0f))
        sparkleLevel[random(n)] = 255;
    }
    for (int i = 0; i < n; i++) {
      float b = (sparkleLevel[i] / 255.0f) * bri;
      strip.setPixelColor(i, strip.Color(baseR * b, baseG * b, baseB * b));
    }
  } else {
    // Solid.
    float b = bri;
    for (int i = 0; i < n; i++)
      strip.setPixelColor(i, strip.Color(baseR * b, baseG * b, baseB * b));
  }
  strip.show();
}

void sendReady() {
  OBSerial.printf("/sys/ready light %u\n", DEVICE_ID);
}

void handleLine(const char *line) {
  if (strcmp(line, "ACK") == 0) { sendReady(); return; }

  char prefix[32];
  int a, b, c;

  // /light/<ID>/num N
  snprintf(prefix, sizeof(prefix), "/light/%u/num", DEVICE_ID);
  if (strncmp(line, prefix, strlen(prefix)) == 0) {
    if (sscanf(line + strlen(prefix), "%d", &a) == 1) {
      numLeds = constrain(a, 1, MAX_LEDS);
    }
    return;
  }

  // /light/<ID>/color R G B
  snprintf(prefix, sizeof(prefix), "/light/%u/color", DEVICE_ID);
  if (strncmp(line, prefix, strlen(prefix)) == 0) {
    if (sscanf(line + strlen(prefix), "%d %d %d", &a, &b, &c) == 3) {
      baseR = constrain(a, 0, 255);
      baseG = constrain(b, 0, 255);
      baseB = constrain(c, 0, 255);
    }
    return;
  }

  // /light/<ID>/effect M P B
  snprintf(prefix, sizeof(prefix), "/light/%u/effect", DEVICE_ID);
  if (strncmp(line, prefix, strlen(prefix)) == 0) {
    int m; float pr, br;
    if (sscanf(line + strlen(prefix), "%d %f %f", &m, &pr, &br) == 3) {
      mode = (uint8_t)constrain(m, 0, 5);
      param = constrain(pr, 0.0f, 1.0f);
      brightness = constrain(br, 0.0f, 1.0f);
    }
    return;
  }
}

void setup() {
#if !ARDUINO_USB_MODE
  // Descriptor strings MUST be set before USB.begin() (see OB Wheel note).
  char sn[4];
  snprintf(sn, sizeof(sn), "%u", DEVICE_ID);
  USB.manufacturerName("OpenBlocks");
  USB.productName("OB LED Strip");
  USB.serialNumber(sn);
  USBSerial.begin();
  USB.begin();
#else
  Serial.begin(115200);
#endif

  strip.begin();
  strip.clear();
  strip.show();

  sendReady();
  lastReadyAt = millis();
}

void loop() {
  uint32_t now = millis();

  // ── Incoming commands ──
  while (OBSerial.available()) {
    char ch = (char)OBSerial.read();
    if (ch == '\n' || ch == '\r') {
      if (lineLen > 0) {
        lineBuf[lineLen] = '\0';
        handleLine(lineBuf);
        lineLen = 0;
      }
    } else if (lineLen < sizeof(lineBuf) - 1) {
      lineBuf[lineLen++] = ch;
    } else {
      lineLen = 0; // overflow — drop the line
    }
  }

  // ── Render ~60 fps. Pulse rate scales with `param` (slow..fast). ──
  if (now - lastFrameAt >= 16) {
    lastFrameAt = now;
    // Phase period: param 0 → ~4 s, param 1 → ~0.3 s.
    float periodMs = 4000.0f - param * 3700.0f;
    float phase = fmodf((float)now, periodMs) / periodMs;
    renderStrip(phase);
  }

  // ── Keep-alive ──
  if (now - lastReadyAt >= 1000) {
    sendReady();
    lastReadyAt = now;
  }
}
