use std::{mem, ptr};
use libc::{self, c_char};
use std::slice;
use std::cell::RefCell;
use std::collections::VecMap;

use super::get_ladspa_descriptor;

// essentially ladspa.h API translated to rust.
pub mod ladspa {
    use libc::{c_void, c_char};

    pub type Data = f32;
    pub type Properties = i32;
    pub type PortDescriptor = i32;
    pub type PortRangeHintDescriptor = i32;

    pub type Handle = *mut c_void;

    #[repr(C)]
    #[derive(Copy)]
    pub struct PortRangeHint {
        pub hint_descriptor: PortRangeHintDescriptor,
        pub lower_bound: Data,
        pub upper_bound: Data,
    }

    #[repr(C)]
    #[allow(missing_copy_implementations)] // Remove this for a fun warning/suggestion cycle!
    pub struct Descriptor {
        pub unique_id: u64,
        pub label: *const c_char,
        pub properties: Properties,
        pub name: *const c_char,
        pub maker: *const c_char,
        pub copyright: *const c_char,
        pub port_count: u64,
        pub port_descriptors: *mut PortDescriptor,
        pub port_names: *mut *const c_char,
        pub port_range_hints: *mut PortRangeHint,
        pub implementation_data: *mut c_void,
        pub instantiate: extern "C" fn(descriptor: *const Descriptor, sample_rate: u64) -> Handle,
        pub connect_port: extern "C" fn(instance: Handle, port: u64, data_location: *mut Data),
        pub activate: extern "C" fn(instance: Handle),
        pub run: extern "C" fn(instance: Handle, sample_count: u64),
        pub run_adding: extern "C" fn(instance: Handle, sample_count: u64),
        pub set_run_adding_gain: extern "C" fn(instance: Handle, gain: Data),
        pub deactivate: extern "C" fn(instance: Handle),
        pub cleanup: extern "C" fn(instance: Handle),
    }

    pub const PROPERTY_REALTIME: Properties = 0x1;
    pub const PROPERTY_INPLACE_BROKEN: Properties = 0x2;
    pub const PROPERTY_HARD_RT_CAPABLE: Properties = 0x4;

    pub const PORT_INPUT: PortDescriptor = 0x1;
    pub const PORT_OUTPUT: PortDescriptor = 0x2;
    pub const PORT_CONTROL: PortDescriptor = 0x4;
    pub const PORT_AUDIO: PortDescriptor = 0x8;

    pub const HINT_BOUNDED_BELOW: PortRangeHintDescriptor = 0x1;
    pub const HINT_BOUNDED_ABOVE: PortRangeHintDescriptor = 0x2;
    pub const HINT_TOGGLED: PortRangeHintDescriptor = 0x4;
    pub const HINT_SAMPLE_RATE: PortRangeHintDescriptor = 0x8;
    pub const HINT_LOGARITHMIC: PortRangeHintDescriptor = 0x10;
    pub const HINT_INTEGER: PortRangeHintDescriptor = 0x20;
    pub const HINT_DEFAULT_MINIMUM: PortRangeHintDescriptor = 0x40;
    pub const HINT_DEFAULT_LOW: PortRangeHintDescriptor = 0x80;
    pub const HINT_DEFAULT_MIDDLE: PortRangeHintDescriptor = 0xC0;
    pub const HINT_DEFAULT_HIGH: PortRangeHintDescriptor = 0x100;
    pub const HINT_DEFAULT_MAXIMUM: PortRangeHintDescriptor = 0x140;
    pub const HINT_DEFAULT_0: PortRangeHintDescriptor = 0x200;
    pub const HINT_DEFAULT_1: PortRangeHintDescriptor = 0x240;
    pub const HINT_DEFAULT_100: PortRangeHintDescriptor = 0x280;
    pub const HINT_DEFAULT_440: PortRangeHintDescriptor = 0x2C0;
}

unsafe fn alloc<T>(num: u64) -> *mut T {
    let ptr: *mut T = mem::transmute(libc::malloc(num * mem::size_of::<T>() as u64));
    if ptr == ptr::null_mut() {
        panic!("malloc returned null!");
    }
    ptr
}

unsafe fn free<T>(x: *const T) {
    libc::free(mem::transmute(x));
}

unsafe fn make_c_str(s: &'static str) -> *const c_char {
    let c_str: *mut c_char = alloc::<c_char>(s.len() as u64 + 1);
    ptr::copy_memory(c_str, mem::transmute(s.as_ptr()), s.len());
    slice::from_raw_parts_mut(c_str, s.len() + 1)[s.len()] = 0; // add the null terminator
    c_str
}

static mut init_done: bool = false;
static mut num_descriptors: u64 = 0;
static mut descriptors: Option<*mut *mut ladspa::Descriptor> = None;
static MAX_DESCRIPTORS: u64 = 32;

// It seems that ladspa_descriptor is deleted during link time optimization unless we
// call it from somewhere.
#[allow(dead_code)]
unsafe fn _lto_workaround() {
    ladspa_descriptor(0);
}

#[no_mangle]
// Exported so the plugin is recognised by ladspa hosts.
pub unsafe extern "C" fn ladspa_descriptor(index: u64) -> *mut ladspa::Descriptor {
    if !init_done {
        libc::atexit(global_destruct);
        descriptors = Some(alloc(MAX_DESCRIPTORS));
        init_done = true;
    }

    // If it's already been generated, return the cached copy.
    if index < num_descriptors {
        return *descriptors.unwrap().offset(index as isize);
    }

    match get_ladspa_descriptor(index) {
        Some(plugin) => {
            let desc: &mut ladspa::Descriptor = mem::transmute(alloc::<ladspa::Descriptor>(1));

            // the following is ok because none of the fields in the descriptor have
            // destructors. if they did rust would try to drop null pointers on write.

            // copy data fields
            desc.unique_id = plugin.unique_id;
            desc.label = make_c_str(plugin.label);
            desc.properties = plugin.properties.bits();
            desc.name = make_c_str(plugin.name);
            desc.maker = make_c_str(plugin.maker);
            desc.copyright = make_c_str(plugin.copyright);
            desc.port_count = plugin.ports.len() as u64;
            desc.port_descriptors = alloc::<ladspa::PortDescriptor>(desc.port_count);
            desc.port_names = alloc::<*const c_char>(desc.port_count);
            desc.port_range_hints = alloc::<ladspa::PortRangeHint>(desc.port_count);
            for i in 0..desc.port_count as usize {
                *desc.port_descriptors.offset(i as isize)
                    = plugin.ports[i].desc as i32;

                *desc.port_names.offset(i as isize)
                    = make_c_str(plugin.ports[i].name);

                let port = &plugin.ports[i];
                *desc.port_range_hints.offset(i as isize)
                    = ladspa::PortRangeHint {
                        hint_descriptor: port.hint.map(|x| x.bits()).unwrap_or(0) |
                            port.default.map(|x| x as i32).unwrap_or(0) |
                            port.lower_bound.map(|_| ladspa::HINT_BOUNDED_BELOW).unwrap_or(0) |
                            port.upper_bound.map(|_| ladspa::HINT_BOUNDED_ABOVE).unwrap_or(0),
                        lower_bound: port.lower_bound.unwrap_or(0_f32),
                        upper_bound: port.upper_bound.unwrap_or(0_f32),
                    };
            }

            // implementation_data holds the original rustic descriptor
            desc.implementation_data = mem::transmute(alloc::<super::PluginDescriptor>(1));
            ptr::write(mem::transmute::<_, *mut super::PluginDescriptor>
                    (desc.implementation_data), plugin);

            // attach functions
            desc.instantiate = instantiate;
            desc.connect_port = connect_port;
            desc.run = run;
            desc.cleanup = cleanup;
            // u8 is arbitrary, just need some type here
            desc.run_adding = mem::transmute(ptr::null::<*const u8>());
            desc.set_run_adding_gain = mem::transmute(ptr::null::<*const u8>());
            desc.activate = activate;
            desc.deactivate = deactivate;

            // store in global descriptor table
            let ptr = mem::transmute(desc);
            *descriptors.unwrap().offset(num_descriptors as isize) = ptr;
            num_descriptors += 1;
            if num_descriptors >= MAX_DESCRIPTORS {
                panic!("The program tried to define more than the max supported number of descriptors currently supported - this usually means you forgot to return None at some point in get_ladspa_descriptor.");
            }

            ptr
        }
        None => ptr::null_mut()
    }
}

// these next two should free everything allocated in ladspa_descriptor - checked with valgrind.
extern "C" fn global_destruct() {
    unsafe {
        if !init_done {
            return;
        }
        for i in 0..num_descriptors {
            free_descriptor(*descriptors.unwrap().offset(i as isize));
        }
        free(descriptors.unwrap());
    }
}

unsafe fn free_descriptor(ptr: *mut ladspa::Descriptor) {
    let desc: &mut ladspa::Descriptor = mem::transmute(ptr);
    free(desc.label);
    free(desc.name);
    free(desc.maker);
    free(desc.copyright);
    free(desc.port_descriptors);
    for i in 0..desc.port_count {
        free(*desc.port_names.offset(i as isize));
    }
    free(desc.port_names);
    free(desc.port_range_hints);
    let rust_desc: *mut super::PluginDescriptor = mem::transmute(desc.implementation_data);
    drop(ptr::read(rust_desc));
    free(desc.implementation_data);
    free(ptr);
}

// The handle that is given to ladspa.
struct Handle<'a> {
    descriptor: &'static super::PluginDescriptor,
    plugin: Box<super::Plugin + 'static>,
    port_map: VecMap<super::PortConnection<'a>>,
    ports: Vec<&'a super::PortConnection<'a>>,
}

extern "C" fn instantiate(descriptor: *const ladspa::Descriptor, sample_rate: u64) -> ladspa::Handle {
    unsafe {
        let desc: &mut ladspa::Descriptor = mem::transmute(descriptor);

        let rust_desc: &super::PluginDescriptor = mem::transmute(desc.implementation_data);
        let rust_plugin = (rust_desc.new)(rust_desc, sample_rate);
        let port_map: VecMap<super::PortConnection> = VecMap::new();
        let ports: Vec<&super::PortConnection> = Vec::new();

        let handle: &mut Handle = mem::transmute(alloc::<Handle>(1));
        ptr::write(mem::transmute(&handle.descriptor), rust_desc);
        ptr::write(mem::transmute(&handle.plugin), rust_plugin);
        ptr::write(mem::transmute(&handle.port_map), port_map);
        ptr::write(mem::transmute(&handle.ports), ports);
        mem::transmute(handle)
    }
}

extern "C" fn connect_port(instance: ladspa::Handle, port_num: u64, data_location: *mut ladspa::Data) {
    unsafe {
        let handle: &mut Handle = mem::transmute(instance);

        let port = handle.descriptor.ports[port_num as usize];

        // Create appropriate pointers to port data. Mutable locations are wrapped in refcells.
        let data = match port.desc {
            super::PortDescriptor::AudioInput => {
                super::PortData::AudioInput( // Initially create a size 0 slice because we don't know how big
                    slice::from_raw_parts(data_location, 0)) // it will be yet.
            },
            super::PortDescriptor::AudioOutput => {
                super::PortData::AudioOutput(RefCell::new( // Same here.
                    slice::from_raw_parts_mut(data_location, 0)))
            },
            super::PortDescriptor::ControlInput => {
                super::PortData::ControlInput(mem::transmute(data_location))
            },
            super::PortDescriptor::ControlOutput => {
                super::PortData::ControlOutput(RefCell::new(mem::transmute(data_location)))
            },
            super::PortDescriptor::Invalid => panic!("Invalid port descriptor!"),
        };

        let conn = super::PortConnection {
            port: port,
            data: data,
        };
        handle.port_map.insert(port_num as usize, conn);

        // Depends on the assumption that ports will be recreated whenever port_map changes
        let handle_ptr: *mut Handle = mem::transmute(instance);
        if handle.port_map.len() == handle.descriptor.ports.len() {
            (*handle_ptr).ports = (0..handle.port_map.len()).map(|i| &handle.port_map[i]).collect();
        }
    }
}

extern "C" fn run(instance: ladspa::Handle, sample_count: u64) {
    unsafe {
        let handle: &mut Handle = mem::transmute(instance);
        for (_, port) in handle.port_map.iter_mut() {
            match port.data {
                super::PortData::AudioOutput(ref mut data) => {
                    let ptr = data.borrow_mut().as_mut_ptr();
                    *data.borrow_mut() = slice::from_raw_parts_mut(ptr, sample_count as usize);
                },
                super::PortData::AudioInput(ref mut data) => {
                    let ptr = data.as_ptr();
                    *data = slice::from_raw_parts(ptr, sample_count as usize);
                },
                _ => { }
            }
        }
        handle.plugin.run(sample_count as usize, handle.ports.as_slice());
    }
}

extern "C" fn activate(instance: ladspa::Handle) {
    unsafe{
        let handle: &mut Handle = mem::transmute(instance);
        handle.plugin.activate();
    }
}
extern "C" fn deactivate(instance: ladspa::Handle) {
    unsafe {
        let handle: &mut Handle = mem::transmute(instance);
        handle.plugin.deactivate();
    }
}

/*
extern "C" fn run_adding(instance: ladspa::Handle, sample_count: u64) {
}
extern "C" fn set_run_adding_gain(instance: ladspa::Handle, gain: ladspa::Data) {
}
*/

extern "C" fn cleanup(instance: ladspa::Handle) {
    unsafe {
        let handle: *mut Handle = mem::transmute(instance);
        drop(ptr::read(handle));
        free(instance);
    }
}

