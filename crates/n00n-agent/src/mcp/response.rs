use futures_lite::AsyncReadExt;
use isahc::AsyncBody;

pub const MAX_RESPONSE_BODY: usize = 1_048_576;
const INITIAL_BODY_CAPACITY: usize = 8 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ResponseReadError {
    #[error("response body exceeded the {limit_bytes} byte limit")]
    TooLarge { limit_bytes: usize },
    #[error("failed to read response body: {0}")]
    Read(#[source] std::io::Error),
    #[error("response body was not valid UTF-8: {0}")]
    Utf8(#[source] std::string::FromUtf8Error),
}

pub async fn read_bounded_text(
    body: &mut AsyncBody,
    limit_bytes: usize,
) -> Result<String, ResponseReadError> {
    let read_limit = u64::try_from(limit_bytes)
        .unwrap_or_else(|_| u64::MAX)
        .saturating_add(1);
    let mut bytes = Vec::with_capacity(limit_bytes.min(INITIAL_BODY_CAPACITY));
    body.take(read_limit)
        .read_to_end(&mut bytes)
        .await
        .map_err(ResponseReadError::Read)?;
    if bytes.len() > limit_bytes {
        return Err(ResponseReadError::TooLarge { limit_bytes });
    }
    String::from_utf8(bytes).map_err(ResponseReadError::Utf8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use isahc::AsyncBody;

    #[test]
    fn bounded_reader_accepts_exact_limit() {
        smol::block_on(async {
            let mut body = AsyncBody::from("abcd".as_bytes().to_vec());
            assert_eq!(read_bounded_text(&mut body, 4).await.unwrap(), "abcd");
        });
    }

    #[test]
    fn bounded_reader_rejects_limit_plus_one() {
        smol::block_on(async {
            let mut body = AsyncBody::from("abcde".as_bytes().to_vec());
            assert!(matches!(
                read_bounded_text(&mut body, 4).await,
                Err(ResponseReadError::TooLarge { limit_bytes: 4 })
            ));
        });
    }

    #[test]
    fn bounded_reader_rejects_invalid_utf8() {
        smol::block_on(async {
            let mut body = AsyncBody::from(vec![0xff]);
            assert!(matches!(
                read_bounded_text(&mut body, 4).await,
                Err(ResponseReadError::Utf8(_))
            ));
        });
    }
}
