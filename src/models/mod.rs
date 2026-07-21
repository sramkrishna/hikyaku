mod room_object;
mod rolodex_entry_object;
mod notification_object;

pub use room_object::RoomObject;
pub use rolodex_entry_object::RolodexEntryObject;
pub use notification_object::NotificationObject;

// Re-exported from the hikyaku-timeline crate. Keeps existing
// `crate::models::MessageObject` and `crate::models::Timeline`
// paths in the bin crate working while the actual code lives in
// `crates/timeline/`.
pub use hikyaku_timeline::{MessageObject, Timeline};
