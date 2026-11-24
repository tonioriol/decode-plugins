//! Embeddings stored as one `f32` scale and `dim` signed bytes per row: a quarter of `f32` rows, and cosine order is
//! kept to well within the ranking's needs (rows are L2-normalized, so every component is in `[-1, 1]`).

/// `row` quantized: its scale (`max |x| / 127`) followed by each component divided by it, rounded.
pub fn quantize(row: &[f32], out: &mut Vec<u8>) {
    let max = row.iter().fold(0.0_f32, |max, x| max.max(x.abs()));
    let scale = max / 127.0;
    out.extend_from_slice(&scale.to_le_bytes());
    for x in row {
        let q = if scale > 0.0 { (x / scale).round() } else { 0.0 };
        #[expect(clippy::cast_possible_truncation, reason = "clamped to the i8 range first")]
        let q = q.clamp(-127.0, 127.0) as i8;
        out.push(q.to_le_bytes()[0]);
    }
}

/// The bytes one quantized row of `dim` components takes.
pub const fn row_bytes(dim: usize) -> usize {
    4 + dim
}

/// The dot product of a quantized query (`scale`, `values`) and one stored row.
pub fn dot(scale: f32, values: &[i8], row: &[u8]) -> f32 {
    let Some((row_scale, components)) = row.split_first_chunk::<4>() else {
        return 0.0;
    };
    let sum: i32 = values
        .iter()
        .zip(components)
        .map(|(a, b)| i32::from(*a) * i32::from(i8::from_le_bytes([*b])))
        .sum();
    #[expect(
        clippy::cast_precision_loss,
        reason = "a sum of at most dim * 127^2 is exact in f32's range"
    )]
    let sum = sum as f32;
    sum * scale * f32::from_le_bytes(*row_scale)
}

/// The dot product of an unquantized query and one stored row.
pub fn dot_f32(query: &[f32], row: &[u8]) -> f32 {
    let Some((row_scale, components)) = row.split_first_chunk::<4>() else {
        return 0.0;
    };
    let sum: f32 = query
        .iter()
        .zip(components)
        .map(|(a, b)| a * f32::from(i8::from_le_bytes([*b])))
        .sum();
    sum * f32::from_le_bytes(*row_scale)
}

/// Whether a stored row holds a vector: a written row of a normalized vector has a positive scale, and a row not
/// written yet reads as zeros.
pub fn is_written(row: &[u8]) -> bool {
    row.first_chunk::<4>()
        .is_some_and(|scale| f32::from_le_bytes(*scale) > 0.0)
}

/// A query row quantized for [`dot`].
pub fn quantize_query(row: &[f32]) -> (f32, Vec<i8>) {
    let mut bytes = Vec::with_capacity(row_bytes(row.len()));
    quantize(row, &mut bytes);
    let (scale, values) = bytes
        .split_first_chunk::<4>()
        .map_or((0.0, &[][..]), |(scale, values)| {
            (f32::from_le_bytes(*scale), values)
        });
    (scale, values.iter().map(|b| i8::from_le_bytes([*b])).collect())
}

#[cfg(test)]
mod tests {
    use super::{dot, quantize, quantize_query};

    #[test]
    fn quantized_dot_tracks_the_float_dot() {
        let a = [0.6_f32, -0.48, 0.0, 0.64];
        let b = [0.1_f32, 0.7, -0.7, 0.1];
        let mut row = Vec::new();
        quantize(&b, &mut row);
        let (scale, query) = quantize_query(&a);
        let exact: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
        assert!((dot(scale, &query, &row) - exact).abs() < 0.01, "{exact}");
        let mut zero = Vec::new();
        quantize(&[0.0; 4], &mut zero);
        assert_eq!(dot(scale, &query, &zero), 0.0);
    }
}
