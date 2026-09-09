//! Bounded array selections shared by the viewer and debugger adapters.

pub(crate) const PAGE_LIMIT: usize = 512;
pub(crate) const BATCH_LIMIT: usize = 64;
pub(crate) const RANK_LIMIT: usize = 15;
pub(crate) const SEQUENTIAL_LIMIT: u64 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArrayOrder {
    RowMajor,
    ColumnMajor,
    Sequence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArrayShape {
    /// Inclusive bounds in source-language dimension order, not GDB type nesting order.
    pub(crate) bounds: Vec<(i64, i64)>,
    pub(crate) order: ArrayOrder,
    pub(crate) sequential: bool,
    pub(crate) length_known: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AxisRange {
    /// A native array index. Fortran indices may be negative or start above zero.
    pub(crate) start: i64,
    pub(crate) count: u64,
    pub(crate) stride: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArraySlice {
    pub(crate) axes: Vec<AxisRange>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArrayPage {
    pub(crate) slice: ArraySlice,
    /// Ordinal within the selected slice, traversed in the array's storage order.
    pub(crate) offset: u64,
    pub(crate) count: usize,
}

impl ArrayShape {
    pub(crate) fn full_slice(&self) -> Result<ArraySlice, &'static str> {
        let axes = self
            .bounds
            .iter()
            .map(|&(lower, upper)| {
                let length = (i128::from(upper) - i128::from(lower) + 1).max(0);

                Ok(AxisRange {
                    start: lower,
                    count: u64::try_from(length).map_err(|_| "Array dimension is too large")?,
                    stride: 1,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let slice = ArraySlice { axes };
        slice.validate(self)?;
        Ok(slice)
    }

    pub(crate) fn description(&self) -> String {
        let bounds = self
            .bounds
            .iter()
            .map(|(lower, upper)| format!("{lower}:{upper}"))
            .collect::<Vec<_>>()
            .join(", ");

        let order = match self.order {
            ArrayOrder::RowMajor => "Row-major",
            ArrayOrder::ColumnMajor => "Column-major",
            ArrayOrder::Sequence => "Sequence",
        };

        let restriction = if !self.length_known {
            " · Length unknown, traversal bounded to 4096 elements"
        } else if self.sequential {
            " · Sequential printer, seek limited to 4096 elements"
        } else {
            ""
        };

        let label = if self.length_known {
            "Bounds"
        } else {
            "Inspection window"
        };
        format!("{label} ({bounds}) · {order}{restriction}")
    }
}

impl ArraySlice {
    pub(crate) fn total(&self) -> Result<u64, &'static str> {
        if self.axes.iter().any(|axis| axis.count == 0) {
            return Ok(0);
        }

        self.axes.iter().try_fold(1_u64, |total, axis| {
            total.checked_mul(axis.count).ok_or("Slice size overflows")
        })
    }

    pub(crate) fn validate(&self, shape: &ArrayShape) -> Result<u64, &'static str> {
        if self.axes.is_empty()
            || self.axes.len() > RANK_LIMIT
            || self.axes.len() != shape.bounds.len()
        {
            return Err("Slice dimensions do not match the array");
        }

        for (axis, &(lower, upper)) in self.axes.iter().zip(&shape.bounds) {
            if axis.stride == 0 {
                return Err("Stride must not be zero");
            }

            if axis.count == 0 {
                if axis.start != lower && !(lower..=upper).contains(&axis.start) {
                    return Err("Slice start is outside the dimension bounds");
                }

                continue;
            }

            let last =
                i128::from(axis.start) + i128::from(axis.count - 1) * i128::from(axis.stride);

            if !(lower..=upper).contains(&axis.start)
                || !(i128::from(lower)..=i128::from(upper)).contains(&last)
            {
                return Err("Slice extends outside the dimension bounds");
            }
        }

        self.total()
    }
}

impl ArrayPage {
    pub(crate) fn validate(&self, shape: &ArrayShape) -> Result<u64, &'static str> {
        let total = self.slice.validate(shape)?;

        if self.count == 0 || self.count > PAGE_LIMIT {
            return Err("Page size must be between 1 and 512");
        }

        if self.offset > total || (total > 0 && self.offset == total) {
            return Err("Page is outside the selected slice");
        }

        if shape.sequential && total > 0 {
            let axis = self
                .slice
                .axes
                .first()
                .ok_or("Missing sequence dimension")?;
            let last_offset = self.offset + (total - self.offset).min(self.count as u64) - 1;
            let first = i128::from(axis.start) + i128::from(self.offset) * i128::from(axis.stride);
            let last = i128::from(axis.start) + i128::from(last_offset) * i128::from(axis.stride);

            if shape.bounds.len() != 1
                || !(0..i128::from(SEQUENTIAL_LIMIT)).contains(&first)
                || !(0..i128::from(SEQUENTIAL_LIMIT)).contains(&last)
            {
                return Err(
                    "This printer requires sequential traversal. Indices must be below 4096",
                );
            }
        }

        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slices_preserve_negative_bounds_and_validate_all_dimensions() {
        let shape = ArrayShape {
            bounds: vec![(-2, 2), (4, 9)],
            order: ArrayOrder::ColumnMajor,
            sequential: false,
            length_known: true,
        };

        let mut slice = shape.full_slice().unwrap();
        assert_eq!(slice.total(), Ok(30));
        slice.axes[0] = AxisRange {
            start: 2,
            count: 3,
            stride: -2,
        };
        assert_eq!(slice.validate(&shape), Ok(18));
        slice.axes[0].count = 4;
        assert!(slice.validate(&shape).is_err());
        slice.axes[0].count = 1;
        slice.axes[1].stride = 0;
        assert!(slice.validate(&shape).is_err());
    }

    #[test]
    fn slice_arithmetic_is_checked_before_dispatch() {
        let shape = ArrayShape {
            bounds: vec![(i64::MIN, i64::MAX)],
            order: ArrayOrder::RowMajor,
            sequential: false,
            length_known: true,
        };

        assert!(shape.full_slice().is_err());
        let mut shape = ArrayShape {
            bounds: vec![(0, i64::MAX); 2],
            ..shape
        };
        assert!(shape.full_slice().is_err());
        shape.bounds.push((1, 0));
        let slice = shape.full_slice().unwrap();
        assert_eq!(slice.total(), Ok(0));
        let page = ArrayPage {
            slice,
            offset: 0,
            count: PAGE_LIMIT,
        };
        assert_eq!(page.validate(&shape), Ok(0));
        assert!(
            ArrayPage {
                count: PAGE_LIMIT + 1,
                ..page
            }
            .validate(&shape)
            .is_err()
        );
    }

    #[test]
    fn sequential_seek_budget_applies_to_page_coordinates_not_slice_length() {
        let shape = ArrayShape {
            bounds: vec![(0, 8191)],
            order: ArrayOrder::Sequence,
            sequential: true,
            length_known: true,
        };

        let mut page = ArrayPage {
            slice: shape.full_slice().unwrap(),
            offset: 0,
            count: 4,
        };
        assert_eq!(page.validate(&shape), Ok(8192));
        page.offset = 4093;
        assert!(page.validate(&shape).is_err());
        page.offset = 0;
        page.slice.axes[0] = AxisRange {
            start: 4095,
            count: 4096,
            stride: -1,
        };
        assert_eq!(page.validate(&shape), Ok(4096));
        page.offset = 4095;
        assert_eq!(page.validate(&shape), Ok(4096));
        page.offset = 4096;
        assert!(page.validate(&shape).is_err());
    }
}
