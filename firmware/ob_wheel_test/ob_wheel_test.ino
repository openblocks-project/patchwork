// OB Wheel — serial test sketch (step 1 of 2)
//
// Board: PCBCUPID Glyph S3 (ESP32-S3)
// Wiring:
//   Encoder CLK → A1 (GPIO1)
//   Encoder DT  → A2 (GPIO2)
//   Encoder SW  → SDA (GPIO4)
//   Encoder VCC → 3V3, GND → GND
//   (LED strip on A6/GPIO6 — unused in this test)
//
// IDE: Tools → USB CDC On Boot: Enabled (default HW CDC is fine here).
// Open the serial monitor at 115200. Turning the wheel prints one line per
// detent with direction and running position; the switch prints press/release.

const int PIN_CLK = 1;  // A1
const int PIN_DT  = 2;  // A2
const int PIN_SW  = 4;  // SDA

// Full quadrature decode via transition table: 4 sub-steps per detent,
// inherently glitch-immune (invalid transitions score 0), no debounce delay.
// Index = (prevState << 2) | newState, states are (CLK<<1)|DT.
const int8_t QUAD_TABLE[16] = {
   0, -1,  1,  0,
   1,  0,  0, -1,
  -1,  0,  0,  1,
   0,  1, -1,  0,
};

uint8_t prevState = 0;
int8_t  subSteps  = 0;   // accumulates ±4 per detent
long    position  = 0;

bool     swState      = false;  // debounced, true = pressed
bool     swRaw        = false;
uint32_t swChangedAt  = 0;

void setup() {
  Serial.begin(115200);
  pinMode(PIN_CLK, INPUT_PULLUP);
  pinMode(PIN_DT,  INPUT_PULLUP);
  pinMode(PIN_SW,  INPUT_PULLUP);
  prevState = (digitalRead(PIN_CLK) << 1) | digitalRead(PIN_DT);
  Serial.println("OB Wheel test — turn the wheel / press the switch");
}

void loop() {
  // ── Encoder ──
  uint8_t state = (digitalRead(PIN_CLK) << 1) | digitalRead(PIN_DT);
  if (state != prevState) {
    subSteps += QUAD_TABLE[(prevState << 2) | state];
    prevState = state;
    if (subSteps >= 4 || subSteps <= -4) {
      int dir = (subSteps > 0) ? 1 : -1;
      subSteps = 0;
      position += dir;
      Serial.printf("turn %+d  pos %ld\n", dir, position);
    }
  }

  // ── Switch (active low, 25 ms debounce) ──
  bool raw = (digitalRead(PIN_SW) == LOW);
  if (raw != swRaw) {
    swRaw = raw;
    swChangedAt = millis();
  }
  if (swRaw != swState && millis() - swChangedAt > 25) {
    swState = swRaw;
    Serial.printf("click %d\n", swState ? 1 : 0);
  }
}
