use super::*;
const MASS: [usize; 3] = [77, 78, 80];
const COUNTS: [usize; 6] = [86, 87, 88, 89, 90, 91];

pub(super) struct Pmsa003i {
    s: SampleState,
    next: u64,
}
impl Pmsa003i {
    pub fn new(clock: Arc<AtomicU64>, hz: u32) -> Self {
        let mut s = SampleState::new(clock, hz);
        s.time();
        let next = s.now + s.ticks(1_000_000);
        Self { s, next }
    }
    fn packet(&mut self) {
        self.s.regs.fill(0xff);
        self.s.regs[0..4].copy_from_slice(&[0x42, 0x4d, 0, 28]);
        for (i, f) in MASS.into_iter().enumerate() {
            let v = self.s.inputs[f] as u16;
            self.s.put16(4 + i * 2, v as i32);
            self.s.put16(10 + i * 2, v as i32);
            self.s.readings[f] = v as f64;
        }
        for (i, f) in COUNTS.into_iter().enumerate() {
            let v = self.s.inputs[f] as u16;
            self.s.put16(16 + i * 2, v as i32);
            self.s.readings[f] = v as f64;
        }
        self.s.regs[28..30].fill(0);
        let sum: u16 = self.s.regs[..30].iter().map(|b| *b as u16).sum();
        self.s.put16(30, sum as i32);
    }
}
impl RegisterSensor for Pmsa003i {
    fn sync(&mut self) {
        self.s.time();
        if self.s.now >= self.next {
            let period = self.s.ticks(1_000_000);
            let samples = (self.s.now - self.next) / period + 1;
            self.next += samples * period;
            self.packet();
            self.s.publish(samples);
        }
    }
    fn registers(&self) -> [u8; 256] { self.s.regs }
    fn next_address(&self, reg: u8) -> u8 { if reg == 31 { 0 } else { reg.wrapping_add(1) } }
    fn read_ready(&self) -> bool { self.s.generation != 0 }
    fn write(&mut self, _reg: u8, _value: u16) -> bool { false }
    fn set(&mut self, field: u32, value: f64) -> bool {
        let f = field as usize;
        let max = if MASS.contains(&f) { 1000. } else if COUNTS.contains(&f) { 65535. } else { return false; };
        if !value.is_finite() || value.fract() != 0. || !(0. ..=max).contains(&value) { return false; }
        self.sync();
        self.s.inputs[f] = value;
        true
    }
    fn generation(&mut self) -> u32 { self.sync(); self.s.generation }
    fn value(&mut self, field: u32) -> f64 { self.sync(); self.s.value(field) }
}

#[cfg(test)]
mod particulate_tests {
    use super::*;
    fn sensor() -> (Arc<AtomicU64>, Arc<Mutex<Sensor>>, SensorI2c) {
        let clock = Arc::new(AtomicU64::new(0));
        let state = Arc::new(Mutex::new(Sensor::new(SensorConfig {model:54,id:0,sda:1,scl:2,address:0x12,shunt_milliohms:0}, clock.clone(), 1_000_000)));
        let bus = SensorI2c::new(state.clone());
        (clock,state,bus)
    }
    fn values(state:&Arc<Mutex<Sensor>>) {
        for (f,v) in MASS.into_iter().chain(COUNTS).zip([12.,35.,80.,1500.,900.,600.,300.,100.,10.]) {
            assert!(state.lock().unwrap().set(f as u32,v));
        }
    }
    fn read(bus:&mut SensorI2c,n:usize)->Vec<u8>{assert!(bus.start(true));let out=(0..n).map(|_|bus.read()).collect();bus.stop();out}
    #[test] fn particulate_fixed_identity_and_valid_wiring(){
        let (_,state,bus)=sensor();let c=state.lock().unwrap().config;assert!(c.valid());
        assert!(!SensorConfig{address:0x13,..c}.valid());assert!(!SensorConfig{scl:1,..c}.valid());assert!(!SensorConfig{shunt_milliohms:1,..c}.valid());
        assert!(bus.matches_address(0x12,0x12,true));assert!(!bus.matches_address(0x12,0,true));assert!(!bus.matches_address(0x12,0x13,false));assert_eq!(bus.pins(),Some((1,2)));
    }
    #[test] fn particulate_readiness_and_clean_air_defaults(){
        let (clock,state,mut bus)=sensor();assert!(bus.start(false));bus.stop();assert!(!bus.start(true));assert!(state.lock().unwrap().value(77).is_nan());
        clock.store(999_999,Ordering::Relaxed);assert!(!bus.start(true));clock.store(1_000_000,Ordering::Relaxed);
        let bytes=read(&mut bus,32);assert_eq!(&bytes[..4],&[0x42,0x4d,0,28]);assert!(bytes[4..30].iter().all(|v|*v==0));assert_eq!(&bytes[30..],&[0,0xab]);assert_eq!(state.lock().unwrap().value(77),0.);
    }
    #[test] fn particulate_packet_endianness_checksum_mass_pairs_and_explicit_counts(){
        let(clock,state,mut bus)=sensor();values(&state);clock.store(1_000_000,Ordering::Relaxed);let bytes=read(&mut bus,32);
        let expected=[0x42,0x4d,0,28,0,12,0,35,0,80,0,12,0,35,0,80,5,220,3,132,2,88,1,44,0,100,0,10,0,0,4,6];
        assert_eq!(bytes,expected);assert_eq!(state.lock().unwrap().value(86),1500.);assert_eq!(state.lock().unwrap().value(78),35.);
    }
    #[test] fn particulate_updates_once_per_second_without_mass_to_count_inference(){
        let(clock,state,mut bus)=sensor();values(&state);clock.store(1_000_000,Ordering::Relaxed);read(&mut bus,32);
        assert!(state.lock().unwrap().set(78,90.));clock.store(1_999_999,Ordering::Relaxed);assert_eq!(state.lock().unwrap().value(78),35.);
        clock.store(2_000_000,Ordering::Relaxed);assert_eq!(state.lock().unwrap().value(78),90.);assert_eq!(state.lock().unwrap().value(86),1500.);assert_eq!(state.lock().unwrap().generation(),2);
    }
    #[test] fn particulate_snapshot_coherent_across_sample_boundary(){
        let(clock,state,mut bus)=sensor();values(&state);clock.store(1_000_000,Ordering::Relaxed);let original=read(&mut bus,32);
        assert!(bus.start(true));let mut actual=vec![];for _ in 0..16{actual.push(bus.read());}
        state.lock().unwrap().set(77,99.);clock.store(2_000_000,Ordering::Relaxed);state.lock().unwrap().generation();for _ in 0..16{actual.push(bus.read());}bus.stop();assert_eq!(actual,original);
        assert_eq!(&read(&mut bus,32)[4..6],&[0,99]);
    }
    #[test] fn particulate_partial_reads_continue_and_register_pointer_repositions(){
        let(clock,state,mut bus)=sensor();values(&state);clock.store(1_000_000,Ordering::Relaxed);let full=read(&mut bus,32);
        let mut partial=read(&mut bus,11);partial.extend(read(&mut bus,21));assert_eq!(partial,full);
        assert!(bus.start(false));assert!(bus.write(16));bus.stop();assert_eq!(read(&mut bus,2),[5,220]);
        assert!(bus.start(false));assert!(bus.write(0));assert!(!bus.write(9));bus.stop();assert_eq!(read(&mut bus,1),[0x42]);
        assert!(bus.start(false));assert!(!bus.write(32));bus.stop();
    }
    #[test] fn particulate_boundaries_and_reserved_fields(){
        let(_,state,_)=sensor();for field in MASS.into_iter().chain(COUNTS){for bad in [-1.,0.5,f64::NAN,f64::INFINITY,65536.]{assert!(!state.lock().unwrap().set(field as u32,bad));}}
        assert!(!state.lock().unwrap().set(77,1001.));assert!(state.lock().unwrap().set(77,1000.));assert!(state.lock().unwrap().set(86,65535.));assert!(!state.lock().unwrap().set(79,10.));assert!(state.lock().unwrap().value(79).is_nan());
    }
    #[test] fn particulate_instances_have_independent_inputs_and_read_pointers(){
        let(c1,s1,mut b1)=sensor();let(c2,s2,mut b2)=sensor();values(&s1);s2.lock().unwrap().set(78,80.);c1.store(1_000_000,Ordering::Relaxed);c2.store(1_000_000,Ordering::Relaxed);
        read(&mut b1,5);assert_eq!(&read(&mut b2,32)[6..8],&[0,80]);assert_eq!(read(&mut b1,1),[12]);assert_eq!(s1.lock().unwrap().value(78),35.);
    }
}
