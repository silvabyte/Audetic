use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::mpsc;
use std::thread;

use anyhow::{anyhow, Context, Result};
use objc2_core_audio::{
    kAudioHardwarePropertyDefaultInputDevice, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
    AudioObjectAddPropertyListener, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectRemovePropertyListener,
};
use tracing::error;

use super::{ActiveDeviceWatcher, DeviceWatcherBackend, RawDeviceSwitch, RawSwitchSink};

pub(super) fn backend() -> Box<dyn DeviceWatcherBackend> {
    Box::new(CoreAudioBackend)
}

struct CoreAudioBackend;

struct CoreAudioActiveWatcher {
    shutdown_tx: Option<mpsc::Sender<()>>,
    owner_thread: Option<thread::JoinHandle<()>>,
}

impl ActiveDeviceWatcher for CoreAudioActiveWatcher {}

impl Drop for CoreAudioActiveWatcher {
    fn drop(&mut self) {
        drop(self.shutdown_tx.take());
        if let Some(owner_thread) = self.owner_thread.take() {
            if owner_thread.join().is_err() {
                error!("CoreAudio Device Watcher owner thread panicked during shutdown");
            }
        }
    }
}

impl DeviceWatcherBackend for CoreAudioBackend {
    fn start(self: Box<Self>, sink: RawSwitchSink) -> Result<Box<dyn ActiveDeviceWatcher>> {
        let (startup_tx, startup_rx) = mpsc::sync_channel(1);
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        let owner_thread = thread::Builder::new()
            .name("audetic-device-watcher".to_string())
            .spawn(move || {
                // CoreAudio does not guarantee an already-running callback has
                // returned when listener removal completes. Keep this small
                // callback context alive for the process lifetime.
                let context = Box::leak(Box::new(CallbackContext { sink }));
                let listeners = match RegisteredListeners::new(context) {
                    Ok(listeners) => listeners,
                    Err(error) => {
                        let _ = startup_tx.send(Err(error));
                        return;
                    }
                };

                if startup_tx.send(Ok(())).is_err() {
                    listeners.shutdown();
                    return;
                }
                let _ = shutdown_rx.recv();

                listeners.shutdown();
            })
            .context("failed to spawn CoreAudio Device Watcher owner thread")?;

        match startup_rx
            .recv()
            .context("CoreAudio Device Watcher owner thread stopped during startup")?
        {
            Ok(()) => Ok(Box::new(CoreAudioActiveWatcher {
                shutdown_tx: Some(shutdown_tx),
                owner_thread: Some(owner_thread),
            })),
            Err(startup_error) => {
                let _ = owner_thread.join();
                Err(startup_error)
            }
        }
    }
}

struct CallbackContext {
    sink: RawSwitchSink,
}

struct RegisteredListeners {
    context: *mut c_void,
    input: AudioObjectPropertyAddress,
    output: AudioObjectPropertyAddress,
    input_registered: bool,
    output_registered: bool,
}

impl RegisteredListeners {
    fn new(context: &CallbackContext) -> Result<Box<Self>> {
        let context = context as *const CallbackContext as *mut c_void;
        // Listener registration stores pointers to these addresses, so pin
        // their allocation for the full registration lifetime.
        let mut listeners = Box::new(Self {
            context,
            input: property_address(kAudioHardwarePropertyDefaultInputDevice),
            output: property_address(kAudioHardwarePropertyDefaultOutputDevice),
            input_registered: false,
            output_registered: false,
        });

        listeners.add_input()?;
        if let Err(error) = listeners.add_output() {
            if listeners.remove_input() {
                return Err(error);
            }
            Box::leak(listeners);
            return Err(error.context(
                "Default Output registration failed and Default Input listener could not be removed",
            ));
        }
        Ok(listeners)
    }

    fn add_input(&mut self) -> Result<()> {
        let status = unsafe {
            AudioObjectAddPropertyListener(
                system_object(),
                NonNull::from(&mut self.input),
                Some(property_listener),
                self.context,
            )
        };
        check_status(status, "register Default Input listener")?;
        self.input_registered = true;
        Ok(())
    }

    fn add_output(&mut self) -> Result<()> {
        let status = unsafe {
            AudioObjectAddPropertyListener(
                system_object(),
                NonNull::from(&mut self.output),
                Some(property_listener),
                self.context,
            )
        };
        check_status(status, "register Default Output listener")?;
        self.output_registered = true;
        Ok(())
    }

    fn remove(
        context: *mut c_void,
        address: &mut AudioObjectPropertyAddress,
        description: &'static str,
    ) -> bool {
        let status = unsafe {
            AudioObjectRemovePropertyListener(
                system_object(),
                NonNull::from(address),
                Some(property_listener),
                context,
            )
        };
        match check_status(status, description) {
            Ok(()) => true,
            Err(error) => {
                error!(error = %error, "Failed to remove CoreAudio Device Watcher listener; retaining listener storage for safety");
                false
            }
        }
    }

    fn remove_input(&mut self) -> bool {
        if !self.input_registered {
            return true;
        }
        let removed = Self::remove(
            self.context,
            &mut self.input,
            "remove Default Input listener",
        );
        if removed {
            self.input_registered = false;
        }
        removed
    }

    fn remove_output(&mut self) -> bool {
        if !self.output_registered {
            return true;
        }
        let removed = Self::remove(
            self.context,
            &mut self.output,
            "remove Default Output listener",
        );
        if removed {
            self.output_registered = false;
        }
        removed
    }

    fn shutdown(mut self: Box<Self>) {
        let all_removed = self.remove_output() & self.remove_input();
        if !all_removed {
            // CoreAudio may still retain this listener-address storage.
            Box::leak(self);
        }
    }
}

fn property_address(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

fn system_object() -> AudioObjectID {
    kAudioObjectSystemObject as AudioObjectID
}

fn check_status(status: i32, operation: &'static str) -> Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(anyhow!(
            "CoreAudio failed to {operation}: OSStatus {status}"
        ))
    }
}

unsafe extern "C-unwind" fn property_listener(
    _object: AudioObjectID,
    address_count: u32,
    addresses: NonNull<AudioObjectPropertyAddress>,
    context: *mut c_void,
) -> i32 {
    if context.is_null() {
        return 0;
    }

    let context = &*(context as *const CallbackContext);
    let addresses = std::slice::from_raw_parts(addresses.as_ptr(), address_count as usize);
    for address in addresses {
        let event = if address.mSelector == kAudioHardwarePropertyDefaultInputDevice {
            Some(RawDeviceSwitch::Input)
        } else if address.mSelector == kAudioHardwarePropertyDefaultOutputDevice {
            Some(RawDeviceSwitch::Output)
        } else {
            None
        };
        if let Some(event) = event {
            (context.sink)(event);
        }
    }
    0
}
