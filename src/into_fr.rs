use bls12_381::Scalar as Fr;

/// A conversion into an element of the field `Fr`.
pub trait IntoFr: Copy {
    /// Converts `self` to a field element.
    fn into_fr(self) -> Fr;
}

impl IntoFr for Fr {
    fn into_fr(self) -> Fr {
        self
    }
}

impl IntoFr for u64 {
    fn into_fr(self) -> Fr {
        self.into_fr()
    }
}

impl IntoFr for usize {
    fn into_fr(self) -> Fr {
        (self as u64).into_fr()
    }
}

impl IntoFr for i32 {
    fn into_fr(self) -> Fr {
        if self >= 0 {
            (self as u64).into_fr()
        } else {
            ((-self) as u64).into_fr()
        }
    }
}

impl IntoFr for i64 {
    fn into_fr(self) -> Fr {
        if self >= 0 {
            (self as u64).into_fr()
        } else {
            ((-self) as u64).into_fr()
        }
    }
}

impl<'a, T: IntoFr> IntoFr for &'a T {
    fn into_fr(self) -> Fr {
        (*self).into_fr()
    }
}
