use derive_new::new;
use getset::Getters;
use getset::Setters;

#[derive(new, Getters, Setters, PartialEq, Eq)]
#[getset(get = "pub", set = "pub")]
pub struct Measurement {
    value: usize,
}
