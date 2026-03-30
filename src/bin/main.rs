#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use core::{
    cell::RefCell,
    fmt::{self, Write},
};
use crate::alloc::string::ToString;
use bt_hci::controller::ExternalController;
use critical_section::Mutex;
use embassy_executor::Spawner;
use embassy_net::{DhcpConfig, Runner as NetRunner, StackResources};
use embassy_time::{Duration, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::rng::Rng;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::{
    Async,
    Blocking,
    uart::{Config as UartConfig, UartTx},
};
use esp_radio::ble::controller::BleConnector;
use esp_radio::wifi::WifiDevice;
use panic_rtt_target as _;
use smart_leds_trait::{RGB8, SmartLedsWrite};
use trouble_host::prelude::*;
use esp_hal_smartled::{SmartLedsAdapter, smart_led_buffer};
use esp_hal::rmt::Rmt;
use esp_hal::time::Rate;
use picoserve::routing::get;
use picoserve::response::Json;
use picoserve::{AppBuilder, AppRouter};
use static_cell::StaticCell;




extern crate alloc;

const CONNECTIONS_MAX: usize = 1;
const L2CAP_CHANNELS_MAX: usize = 1;

static UART_TX: Mutex<RefCell<Option<UartTx<'static, Blocking>>>> = Mutex::new(RefCell::new(None));
static NET_RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();
static APP: StaticCell<AppRouter<AppProps>> = StaticCell::new();
static RADIO_INIT: StaticCell<esp_radio::Controller<'static>> = StaticCell::new();


#[derive(Copy, Clone, serde::Serialize)]
struct SensorReading {
  temperature: f32,
  humidity: f32,
  pressure: f32,
  gas_resistance: Option<f32>,
}
static TEMP_DATA : Mutex<RefCell<Option<SensorReading>>> = Mutex::new(RefCell::new(None));
type SensorBme680 = bosch_bme680::AsyncBme680<I2c<'static, Async>, embassy_time::Delay>;

fn wifi_client_config() -> esp_radio::wifi::ClientConfig {
    let ssid = option_env!("WIFI_SSID")
        .expect("WIFI_SSID must be set in the build environment");
    let password = option_env!("WIFI_PASSWORD")
        .expect("WIFI_PASSWORD must be set in the build environment");

    esp_radio::wifi::ClientConfig::default()
        .with_ssid(ssid.to_string())
        .with_password(password.to_string())
        .with_auth_method(esp_radio::wifi::AuthMethod::Wpa2Personal)
        .with_channel(6)
}

fn usb_logln(args: fmt::Arguments<'_>) {
    critical_section::with(|cs| {
        if let Some(tx) = UART_TX.borrow_ref_mut(cs).as_mut() {
            let _ = tx.write_fmt(args);
            let _ = tx.write_str("\r\n");
        }
    });
}

macro_rules! usb_logln {
    ($($arg:tt)*) => {
        usb_logln(format_args!($($arg)*))
    };
}



async fn connect_wifi(wifi_controller: &mut esp_radio::wifi::WifiController<'_>, client: esp_radio::wifi::ClientConfig, ) -> Result<(), esp_radio::wifi::WifiError> {

    let mode = esp_radio::wifi::ModeConfig::Client(client);

    //set the config
    usb_logln!("wifi: set_config");
    wifi_controller.set_config(&mode)?;
    //start the controller
    usb_logln!("wifi: start_async");
    wifi_controller.start_async().await?;
    //connect the wifi_controller
    usb_logln!("wifi: connect_async");
    wifi_controller.connect_async().await?;
    usb_logln!("wifi: connect_async succeded");

    Ok(())


}

struct AppProps;

impl AppBuilder for AppProps {
  type PathRouter = impl picoserve::routing::PathRouter;

  fn build_app(self) -> picoserve::Router<Self::PathRouter> {
      picoserve::Router::new()
          .route("/", get(|| async move { "Hello World" }))
          .route("/health", get(|| async move { "200" }))
          .route("/data", get(|| async move {
              let data = critical_section::with(|cs| *TEMP_DATA.borrow_ref(cs));
              Json(data)
          }))
  }
}

static CONFIG: picoserve::Config<embassy_time::Duration> =
    picoserve::Config::new(picoserve::Timeouts {
        start_read_request: None,
        persistent_start_read_request: None,
        read_request: None,
        write: None,
    })
    .keep_connection_alive();

const WEB_TASK_POOL_SIZE: usize = 8;

#[embassy_executor::task(pool_size = WEB_TASK_POOL_SIZE)]
async fn web_task(
    task_id: usize,
    stack: embassy_net::Stack<'static>,
    app: &'static AppRouter<AppProps>,
) -> ! {
    let port = 80;
    let mut tcp_rx_buffer = [0; 1024];
    let mut tcp_tx_buffer = [0; 1024];
    let mut http_buffer = [0; 2048];

    picoserve::Server::new(app, &CONFIG, &mut http_buffer)
        .listen_and_serve(task_id, stack, port, &mut tcp_rx_buffer, &mut tcp_tx_buffer)
        .await
        .into_never()
}

#[embassy_executor::task]
async fn sensor_task(
    mut bme: SensorBme680,
    ) -> ! {
        loop {
            let data = bme.measure().await.expect("failed to measure");

            let new_data = SensorReading{
                temperature: data.temperature,
                humidity: data.humidity,
                pressure: data.pressure,
                gas_resistance: data.gas_resistance,
            };

            critical_section::with(|cs| {
                TEMP_DATA.borrow_ref_mut(cs).replace(new_data);
            });

            Timer::after(Duration::from_secs(1)).await;
            usb_logln!("Data Refreshed")
        }
}


#[embassy_executor::task]
async fn net_task(mut runner: NetRunner<'static, WifiDevice<'static>>) -> ! {
    runner.run().await
}


// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[allow( 
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // generator version: 1.2.0

    rtt_target::rtt_init_defmt!();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    let uart_tx = UartTx::new(peripherals.UART0, UartConfig::default())
        .expect("failed to initialize UART0")
        .with_tx(peripherals.GPIO43);
    critical_section::with(|cs| {
        UART_TX.borrow_ref_mut(cs).replace(uart_tx);
    });

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);
    // COEX needs more RAM - so we've added some more
    esp_alloc::heap_allocator!(size: 64 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0);

    usb_logln!("Embassy initialized!");

    let i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default().with_frequency(Rate::from_khz(100)),
    )
        .expect("failed to initialize I2C0")
        .with_sda(peripherals.GPIO4)
        .with_scl(peripherals.GPIO5)
        .into_async(); 

     let radio_init = RADIO_INIT.init(
        esp_radio::init().expect("Failed to initialize Wi-Fi/BLE controller")
    );

    let (mut wifi_controller, interfaces) =
        esp_radio::wifi::new(radio_init, peripherals.WIFI, Default::default())
            .expect("Failed to initialize Wi-Fi controller");
    let wifi_interface = interfaces.sta;
    

    // find more examples https://github.com/embassy-rs/trouble/tree/main/examples/esp32
    let transport = BleConnector::new(radio_init, peripherals.BT, Default::default()).unwrap();
    let ble_controller = ExternalController::<_, 1>::new(transport);
    let mut resources: HostResources<DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX> =
        HostResources::new();
    let _stack = trouble_host::new(ble_controller, &mut resources);

    let client = wifi_client_config();
  
    connect_wifi(&mut wifi_controller,client)
        .await
        .expect("failed to connect to wifi");

   
    let rng = Rng::new();
    let net_seed = rng.random() as u64 | ((rng.random() as u64) << 32);
    let _tls_seed = rng.random() as u64 | ((rng.random() as u64) << 32);
    let dhcp_config = DhcpConfig::default();
    
    let config = embassy_net::Config::dhcpv4(dhcp_config);
    let (stack, runner) = embassy_net::new(
        wifi_interface,
        config,
        NET_RESOURCES.init(StackResources::<3>::new()),
        net_seed,
    );
    let app = APP.init(AppProps.build_app());

    usb_logln!("net: spawning net_task");
    spawner.spawn(net_task(runner)).ok();
    usb_logln!("net: net_task spawned");

    usb_logln!("net: waiting for DHCP config");
    stack.wait_config_up().await;
    usb_logln!("net: DHCP config acquired");

    if let Some(config_v4) = stack.config_v4() {
        usb_logln!("net: ip={}", config_v4.address.address());
        usb_logln!("net: gateway={:?}", config_v4.gateway);
    } else {
        usb_logln!("net: no IPv4 config available");
    }

    let delay = embassy_time::Delay;

    let mut bme = bosch_bme680::AsyncBme680::new(
        i2c,
        bosch_bme680::DeviceAddress::Primary,
        delay,
        20,
    );

    let config = bosch_bme680::Configuration::default();
    usb_logln!("bme680: initialize start");
    bme.initialize(&config)
        .await
        .expect("could not initilize bme680");
    usb_logln!("bme680: initialize ok");


    //spawn tasks
    usb_logln!("web: spawning web_task on port 80");
    spawner.spawn(web_task(0, stack, app)).unwrap();
    usb_logln!("web: web_task spawned");

    usb_logln!("bme680: spawning sensor_task");
    spawner.spawn(sensor_task(bme)).unwrap();
    usb_logln!("bme680: sensor_task spawned");




    let rmt = Rmt::new(peripherals.RMT, Rate::from_mhz(80)).unwrap();
    let mut led_buffer = smart_led_buffer!(1);
    let mut led = SmartLedsAdapter::new(rmt.channel0, peripherals.GPIO38, &mut led_buffer);

    const LEVEL: u8 = 10;
    let mut color = RGB8 { r: LEVEL, g: 0, b: 0 };

    loop {
        usb_logln!("Blink!");
        led.write([color].into_iter()).unwrap();
        Timer::after(Duration::from_secs(1)).await;
        let tmp: u8 = color.r;
        color.r = color.b;
        color.b = color.g;
        color.g = tmp;

    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v1.0.0/examples
}
