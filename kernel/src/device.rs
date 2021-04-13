use embassy::executor::Spawner;

pub trait Device {
    fn start(&'static self, spawner: Spawner);
}

pub struct DeviceContext<D: Device + 'static> {
    device: &'static D,
    spawner: Spawner,
}

impl<D: Device + 'static> DeviceContext<D> {
    pub fn new(spawner: Spawner, device: &'static D) -> Self {
        Self { spawner, device }
    }

    pub fn device(&self) -> &'static D {
        self.device
    }

    pub fn start(&self) {
        self.device.start(self.spawner)
    }
}
