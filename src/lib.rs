use std::{collections::VecDeque, ffi::c_void, ops::Deref};

use wayland_client::{
    event_enum, global_filter, protocol::wl_seat::WlSeat, sys::client::wl_display, Display,
    EventQueue, GlobalManager, Main,
};
pub use wayland_protocols::unstable::tablet::v2::client::zwp_tablet_tool_v2::Type as ToolType;
use wayland_protocols::unstable::tablet::v2::client::{
    self, zwp_tablet_manager_v2::ZwpTabletManagerV2, zwp_tablet_seat_v2::ZwpTabletSeatV2,
    zwp_tablet_tool_v2::ZwpTabletToolV2,
};

event_enum!(
    SeatFilterEvent |
    SeatEvent => ZwpTabletSeatV2
);

pub struct WaylandTablet {
    inner: WaylandTabletInner,
    display: Display,
    event_queue: EventQueue,
}

impl WaylandTablet {
    pub unsafe fn from_raw_ptr(pointer: *mut c_void) -> Self {
        Self::from_external_display(pointer as *mut wl_display)
    }

    pub fn from_external_display(display_ptr: *mut wl_display) -> Self {
        let waydisp = unsafe { Display::from_external_display(display_ptr) };
        let queue = waydisp.create_event_queue();
        let attached = (*waydisp).clone().attach(queue.token());

        let _globals = GlobalManager::new_with_cb(
            &attached,
            global_filter!(
                [WlSeat, 7, |seat: Main<WlSeat>, mut data: DispatchData| {
                    data.get::<WaylandTabletInner>().unwrap().got_seat(seat)
                }],
                [
                    ZwpTabletManagerV2,
                    1,
                    |manager: Main<ZwpTabletManagerV2>, mut data: DispatchData| {
                        data.get::<WaylandTabletInner>()
                            .unwrap()
                            .got_tablet_manager(manager)
                    }
                ]
            ),
        );

        let mut waytab = Self {
            inner: Default::default(),
            display: waydisp,
            event_queue: queue,
        };

        // Get the current seats and tablet managers
        waytab
            .event_queue
            .sync_roundtrip(&mut waytab.inner, |_, _, _| unreachable!())
            .unwrap();

        waytab
    }

    pub fn events(&mut self) -> Vec<Event> {
        self.display.flush().unwrap();

        if let Some(guard) = self.event_queue.prepare_read() {
            guard.read_events().unwrap();
        }

        self.event_queue
            .dispatch_pending(&mut self.inner, |_, _, _| ())
            .unwrap();

        self.inner.events.drain(..).collect::<Vec<Event>>()
    }
}

/* it seems we can get the TabletManager before the WlSeat, so we have to do
weird gymnastics here */
#[derive(Debug, Default)]
struct WaylandTabletInner {
    unseated_manager: Option<Main<ZwpTabletManagerV2>>,
    seat: Option<WlSeat>,
    tablet_seat: Option<Main<ZwpTabletSeatV2>>,
    next_tool_id: ToolID,
    events: VecDeque<Event>,
}

impl WaylandTabletInner {
    fn send(&mut self, event: Event) {
        self.events.push_back(event)
    }

    fn got_seat(&mut self, seat: Main<WlSeat>) {
        if self.seat.is_some() {
            println!("Warning: Got a second seat, but we can't handle it! Ignoring this seat");
            return;
        } else {
            self.seat = Some(seat.deref().detach());

            if let Some(ref unseated) = self.unseated_manager {
                let tablet_seat = unseated.get_tablet_seat(&seat);
                self.wl_seat(tablet_seat);
            }
        }
    }

    fn got_tablet_manager(&mut self, manager: Main<ZwpTabletManagerV2>) {
        if let Some(ref seat) = self.seat {
            let tablet_seat = manager.get_tablet_seat(seat);
            self.wl_seat(tablet_seat);
        } else {
            self.unseated_manager = Some(manager);
        }
    }

    fn got_tool(&mut self, tool: Main<ZwpTabletToolV2>) {
        let mut ttype = None;
        let mut cap: Capability = Default::default();
        let mut this_tool_id = 0;

        tool.quick_assign(move |_, event, mut data| {
            use client::zwp_tablet_tool_v2::Capability as ToolCap;
            use client::zwp_tablet_tool_v2::Event as ToolEvent;

            let wti = data.get::<WaylandTabletInner>().unwrap();

            match event {
                ToolEvent::Type { tool_type } => {
                    ttype = Some(tool_type);
                }
                ToolEvent::Capability { capability } => match capability {
                    ToolCap::Tilt => cap.tilt = true,
                    ToolCap::Pressure => cap.pressure = true,
                    ToolCap::Distance => cap.distance = true,
                    ToolCap::Rotation => cap.rotation = true,
                    ToolCap::Slider => cap.slider = true,
                    ToolCap::Wheel => cap.wheel = true,
                    _ => (),
                },
                ToolEvent::Done => {
                    this_tool_id = wti.next_tool_id;
                    wti.send(Event::ToolCreated(TabletTool {
                        id: this_tool_id,
                        tool_type: ttype.unwrap(),
                        capability: cap,
                    }));

                    wti.next_tool_id += 1;
                }
                ToolEvent::Removed => return,
                ToolEvent::Down { .. } => wti.send(Event::Down { id: this_tool_id }),
                ToolEvent::Up => wti.send(Event::Up { id: this_tool_id }),
                ToolEvent::Motion { x, y } => wti.send(Event::Moved {
                    id: this_tool_id,
                    x,
                    y,
                }),
                ToolEvent::Pressure { pressure } => wti.send(Event::Pressure {
                    id: this_tool_id,
                    pressure: pressure as f64 / 65535.0,
                }),
                _ => (),
            }
        });
    }

    fn wl_seat(&mut self, seat: Main<ZwpTabletSeatV2>) {
        seat.quick_assign(|_, event, mut data| {
            use client::zwp_tablet_seat_v2::Event as SeatEvent;

            let wti = data.get::<WaylandTabletInner>().unwrap();

            match event {
                SeatEvent::ToolAdded { id } => wti.got_tool(id),
                _ => (),
            }
        });

        self.tablet_seat = Some(seat);
    }
}

#[derive(Copy, Clone, Debug, Default)]
pub struct Capability {
    tilt: bool,
    pressure: bool,
    distance: bool,
    rotation: bool,
    slider: bool,
    wheel: bool,
}

pub type ToolID = u32;

#[derive(Debug)]
pub struct TabletTool {
    id: ToolID,
    tool_type: ToolType,
    capability: Capability,
}

impl PartialEq for TabletTool {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

#[derive(Debug, PartialEq)]
pub enum Event {
    ToolCreated(TabletTool),
    Down {
        id: ToolID,
    },
    Up {
        id: ToolID,
    },
    Moved {
        id: ToolID,
        x: f64,
        y: f64,
    },
    Pressure {
        id: ToolID,
        pressure: f64,
    },
    Distance {
        id: ToolID,
        distance: f64,
    },
    Tilt {
        id: ToolID,
        tilt_x: f64,
        tilt_y: f64,
    },
    Rotation {
        id: ToolID,
        degrees: f64,
    },
    Slider {
        id: ToolID,
        position: f64,
    },
    Wheel {
        id: ToolID,
        degrees: f64,
        clicks: u32,
    },
    //Button
    //Frame
}
