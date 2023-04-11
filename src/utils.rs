pub enum Container {
    Data(Vec<u8>),
}

pub enum Channel {
    Data(usize),
    Image(usize),
}

pub enum ErrCause {
    Data(String),
    Image(String),
}
