# esp32-s3-bme680-http

Small ESP32-S3 firmware project that:

- connects to Wi-Fi with `esp-radio`
- brings up networking with `embassy-net`
- serves a tiny HTTP API with `picoserve`
- reads a BME680 over I2C
- keeps the latest sensor reading in shared state for the HTTP handler

## Current behavior

On boot, the firmware:

1. initializes the ESP32-S3 peripherals
2. connects to Wi-Fi
3. acquires an IPv4 address over DHCP
4. initializes the BME680 sensor
5. starts an HTTP server on port `80`
6. refreshes sensor data in a background task

Available routes:

- `/` returns `Hello World`
- `/health` returns `200`
- `/data` returns the latest sensor reading as JSON

Example `/data` response:

```json
{
  "temperature": 27.97,
  "humidity": 42.56,
  "pressure": 99246.66,
  "gas_resistance": 5243.3335
}
```

## Hardware

Current sensor wiring for the working BME680 setup:

- `VIN` -> `3V3`
- `GND` -> `GND`
- `SCK` -> `GPIO5`
- `SDI` -> `GPIO4`
- `SDO` -> `GND`
- `CS` -> `3V3`
