use derive_new::new;
use getset::CopyGetters;

#[derive(new, CopyGetters)]
#[getset(get_copy = "pub")]
pub struct Options {
    verbose: bool,
}
