use libdictenstein::{
    persistent_serialization_stats, reset_persistent_serialization_stats,
    PersistentSerializationStats,
};

fn main() {
    reset_persistent_serialization_stats();
    let snapshot: PersistentSerializationStats = persistent_serialization_stats();
    assert_eq!(snapshot, PersistentSerializationStats::default());
}
