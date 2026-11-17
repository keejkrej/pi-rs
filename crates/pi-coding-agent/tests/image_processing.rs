//! Port of packages/coding-agent/test/image-processing.test.ts

use pi_coding_agent::utils::image_convert::convert_to_png;
use pi_coding_agent::utils::image_resize::{ImageResizeOptions, ResizedImage, format_dimension_note, resize_image};
use pi_coding_agent::utils::image_resize_core::resize_image_in_process;
use pi_coding_agent::utils::photon::{PhotonImage, load_photon};

const TINY_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACAQMAAABIeJ9nAAAAIGNIUk0AAHomAACAhAAA+gAAAIDoAAB1MAAA6mAAADqYAAAXcJy6UTwAAAAGUExURf8AAP///0EdNBEAAAABYktHRAH/Ai3eAAAAB3RJTUUH6gEOADM5Ddoh/wAAAAxJREFUCNdjYGBgAAAABAABJzQnCgAAACV0RVh0ZGF0ZTpjcmVhdGUAMjAyNi0wMS0xNFQwMDo1MTo1NyswMDowMOnKzHgAAAAldEVYdGRhdGU6bW9kaWZ5ADIwMjYtMDEtMTRUMDA6NTE6NTcrMDA6MDCYl3TEAAAAKHRFWHRkYXRlOnRpbWVzdGFtcAAyMDI2LTAxLTE0VDAwOjUxOjU3KzAwOjAwz4JVGwAAAABJRU5ErkJggg==";

const TINY_JPEG: &str = "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAMCAgMCAgMDAwMEAwMEBQgFBQQEBQoHBwYIDAoMDAsKCwsNDhIQDQ4RDgsLEBYQERMUFRUVDA8XGBYUGBIUFRT/2wBDAQMEBAUEBQkFBQkUDQsNFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBT/wAARCAACAAIDAREAAhEBAxEB/8QAFAABAAAAAAAAAAAAAAAAAAAACf/EABQQAQAAAAAAAAAAAAAAAAAAAAD/xAAVAQEBAAAAAAAAAAAAAAAAAAAGCf/EABQRAQAAAAAAAAAAAAAAAAAAAAD/2gAMAwEAAhEDEQA/AD3VTB3/2Q==";

const TINY_JPEG_2X1: &str = "/9j/4AAQSkZJRgABAgAAAQABAAD/wAARCAABAAIDAREAAhEBAxEB/9sAQwADAgIDAgIDAwMDBAMDBAUIBQUEBAUKBwcGCAwKDAwLCgsLDQ4SEA0OEQ4LCxAWEBETFBUVFQwPFxgWFBgSFBUU/9sAQwEDBAQFBAUJBQUJFA0LDRQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQU/8QAHwAAAQUBAQEBAQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQAAAF9AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3ODk6Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/8QAHwEAAwEBAQEBAQEBAQAAAAAAAAECAwQFBgcICQoL/8QAtREAAgECBAQDBAcFBAQAAQJ3AAECAxEEBSExBhJBUQdhcRMiMoEIFEKRobHBCSMzUvAVYnLRChYkNOEl8RcYGRomJygpKjU2Nzg5OkNERUZHSElKU1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6goOEhYaHiImKkpOUlZaXmJmaoqOkpaanqKmqsrO0tba3uLm6wsPExcbHyMnK0tPU1dbX2Nna4uPk5ebn6Onq8vP09fb3+Pn6/9oADAMBAAIRAxEAPwD4H8Q/8h/Uv+vmX/0M1/o1wJ/ySWU/9g1D/wBNRMOM/wDkp8z/AOv9b/05I//Z";

const MEDIUM_PNG_100X100: &str = "iVBORw0KGgoAAAANSUhEUgAAAGQAAABkCAAAAABVicqIAAAAAmJLR0QA/4ePzL8AAAAHdElNRQfqAQ4AMzkN2iH/AAAAP0lEQVRo3u3NQQEAAAQEMASXXYrz2gqst/Lm4ZBIJBKJRCKRSCQSiUQikUgkEolEIpFIJBKJRCKRSCQSiSTsAP1cAUZeKtreAAAAJXRFWHRkYXRlOmNyZWF0ZQAyMDI2LTAxLTE0VDAwOjUxOjU3KzAwOjAw6crMeAAAACV0RVh0ZGF0ZTptb2RpZnkAMjAyNi0wMS0xNFQwMDo1MTo1NyswMDowMJiXdMQAAAAodEVYdGRhdGU6dGltZXN0YW1wADIwMjYtMDEtMTRUMDA6NTE6NTcrMDA6MDDPglUbAAAAAElFTkSuQmCC";

const LARGE_PNG_200X200: &str = "iVBORw0KGgoAAAANSUhEUgAAAMgAAADIEAIAAAByquWKAAAAIGNIUk0AAHomAACAhAAA+gAAAIDoAAB1MAAA6mAAADqYAAAXcJy6UTwAAAAGYktHRP///////wlY99wAAAAHdElNRQfqAQ4AMzqU03BFAAAEEElEQVR42u3dvWqUURSF4XXMRAyIhSAWQvwnWtjaWusFx9bW1kKD/wELESxEiBjDjJdwysOC57mCXW32+zGcGbtdACpsdi+T8Xz1GABzm91xMl6sHgNgblzcT/Y+rh4DYG6cv0/2H64eA2BOEgI1fHQHaoyzbXIwVo8BMCcJgRqSEKgxft9Lrn5aPQbA3Ph1klw7Wj0GwJwkBGpYWECN8fMiub63egyAORcWUMPCAmqMH3eTG59XjwEwN76/S24+Wj0GwJwkBGpYWECN8e1fcmuzegyAORcWUMPCAmqM09vJ4dfVYwDMjS9vkzuPV48BMCcJgRqeSAZqjA/nyYP91WMAzElCoIYkBGqMk8Pk6HT1GABzm+3L5JIkBApIQqCGj+5AjfHmb/Lk8uoxAOYkIVBDEgI1LCygxmZ7nFyShEABFxZQw8ICaozXf5KnV1aPATDnwgJqWFhADQsLqOFnDUANFxZQw8ICaoxXZ8mzg9VjAMy5sIAaFhZQw3tYQA1/QgHUkIRADUkI1JCEQA1JCNSQhEANFxZQwzcsoIYkBGpIQqCGJARqSEKghiQEalhYQA1/QgHUcGEBNSwsoIYkBGq4sIAaFhZQw8ICaviGBdRwYQE1LCyghiQEariwgBoWFlDDe1hADS+OAjUkIVBDEgI1JCFQQxICNSQhUMOFBdTwDQuoIQmBGpIQqCEJgRqSEKghCYEaFhZQw4ujQA0XFlDDwgJqSEKghgsLqGFhATUsLKCGb1hADRcWUMPCAmpIQqCGCwuoYWEBNbyHBdTw4ihQQxICNSQhUEMSAjUkIVBDEgI1XFhADd+wgBqSEKghCYEakhCoIQmBGpIQqGFhATW8OArUcGEBNSwsoIYkBGq4sIAaFhZQw8ICaviGBdRwYQE1LCyghiQEariwgBoWFlDDe1hADS+OAjUkIVBDEgI1JCFQQxICNSQhUMOFBdTwDQuoIQmBGpIQqCEJgRqSEKghCYEa49lB8ups9RgAc14cBWpIQqCGhQXUkIRADRcWUMPCAmqMp1eS139WjwEw5xsWUEMSAjUsLKCGJARquLCAGhYWUGM8uZy8+bt6DIA5L44CNSQhUMMTyUCNcXSanByuHgNgThICNSQhUGM82E8+nK8eA2DOzxqAGpIQqDHuPE6+vF09BsDcOPyanN5ePQbAnCQEavgdFlBj3Nok3/6tHgNgzoujQA1JCNQYNx8l39+tHgNgbtz4nPy4u3oMgDlJCNSwsIAa4/pe8vNi9RgAc37WANSQhECNce0o+XWyegyAuXH1U/L73uoxAOYkIVDDwgJqjIORnG1XjwEw508ogBqSEKgx9h8m5+9XjwEwN/Y+Jhf3V48BMCcJgRpjPE+2x6vHAJgbSbLbrR4DYO4/GqiSgXN+ksgAAAAldEVYdGRhdGU6Y3JlYXRlADIwMjYtMDEtMTRUMDA6NTE6NTcrMDA6MDDpysx4AAAAJXRFWHRkYXRlOm1vZGlmeQAyMDI2LTAxLTE0VDAwOjUxOjU3KzAwOjAwmJd0xAAAACh0RVh0ZGF0ZTp0aW1lc3RhbXAAMjAyNi0wMS0xNFQwMDo1MTo1NyswMDowMM+CVRsAAAAASUVORK5CYII=";

fn image_bytes(base64_data: &str) -> Vec<u8> {
    pi_js::b64::decode(base64_data)
}

fn app1_segment(payload: &[u8]) -> Vec<u8> {
    let mut segment = vec![0u8; payload.len() + 4];
    segment[0] = 0xff;
    segment[1] = 0xe1;
    let len = (payload.len() + 2) as u16;
    segment[2..4].copy_from_slice(&len.to_be_bytes());
    segment[4..].copy_from_slice(payload);
    segment
}

fn jpeg_with_xmp_before_orientation() -> String {
    let jpeg = pi_js::b64::decode(TINY_JPEG_2X1);
    let xmp = app1_segment(b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>");
    let mut exif_payload = b"Exif\0\0".to_vec();
    exif_payload.extend_from_slice(&hex::decode("49492a0008000000010012010300010000000600000000000000").unwrap());
    let orientation6 = app1_segment(&exif_payload);

    let mut out = Vec::new();
    out.extend_from_slice(&jpeg[0..2]);
    out.extend_from_slice(&xmp);
    out.extend_from_slice(&orientation6);
    out.extend_from_slice(&jpeg[2..]);
    pi_js::b64::encode(&out)
}

mod convert_to_png_tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn should_return_original_data_for_png_input() {
        let result = convert_to_png(TINY_PNG, "image/png").await.unwrap();
        assert_eq!(result.data, TINY_PNG);
        assert_eq!(result.mime_type, "image/png");
    }

    #[tokio::test]
    async fn should_convert_jpeg_to_png() {
        let result = convert_to_png(TINY_JPEG, "image/jpeg").await.unwrap();
        assert_eq!(result.mime_type, "image/png");
        let buffer = pi_js::b64::decode(&result.data);
        assert_eq!(buffer[0], 0x89);
        assert_eq!(buffer[1], 0x50);
        assert_eq!(buffer[2], 0x4e);
        assert_eq!(buffer[3], 0x47);
    }

    #[tokio::test]
    async fn should_apply_exif_orientation_after_an_xmp_app1_segment() {
        let result = convert_to_png(&jpeg_with_xmp_before_orientation(), "image/jpeg")
            .await
            .unwrap();
        let png = pi_js::b64::decode(&result.data);
        let width = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
        let height = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
        assert_eq!(width, 1);
        assert_eq!(height, 2);
    }
}

mod resize_image_tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn should_keep_caller_input_bytes_intact() {
        let input = image_bytes(TINY_PNG);
        let original_len = input.len();
        let original_first = input[0];

        let result = resize_image(
            &input,
            "image/png",
            Some(ImageResizeOptions {
                max_width: Some(100),
                max_height: Some(100),
                max_bytes: Some(1024 * 1024),
                jpeg_quality: None,
            }),
        )
        .await;

        assert!(result.is_some());
        assert_eq!(input.len(), original_len);
        assert_eq!(input[0], original_first);
    }

    #[tokio::test]
    async fn should_return_original_image_if_within_limits() {
        let result = resize_image(
            &image_bytes(TINY_PNG),
            "image/png",
            Some(ImageResizeOptions {
                max_width: Some(100),
                max_height: Some(100),
                max_bytes: Some(1024 * 1024),
                jpeg_quality: None,
            }),
        )
        .await
        .unwrap();

        assert_eq!(result.was_resized, false);
        assert_eq!(result.data, TINY_PNG);
        assert_eq!(result.original_width, 2);
        assert_eq!(result.original_height, 2);
        assert_eq!(result.width, 2);
        assert_eq!(result.height, 2);
    }

    #[tokio::test]
    async fn should_resize_image_exceeding_dimension_limits() {
        let result = resize_image(
            &image_bytes(MEDIUM_PNG_100X100),
            "image/png",
            Some(ImageResizeOptions {
                max_width: Some(50),
                max_height: Some(50),
                max_bytes: Some(1024 * 1024),
                jpeg_quality: None,
            }),
        )
        .await
        .unwrap();

        assert_eq!(result.was_resized, true);
        assert_eq!(result.original_width, 100);
        assert_eq!(result.original_height, 100);
        assert!(result.width <= 50);
        assert!(result.height <= 50);
    }

    #[tokio::test]
    async fn should_resize_image_exceeding_byte_limit() {
        let original_buffer = image_bytes(LARGE_PNG_200X200);
        let original_size = original_buffer.len();

        let result = resize_image(
            &original_buffer,
            "image/png",
            Some(ImageResizeOptions {
                max_width: Some(2000),
                max_height: Some(2000),
                max_bytes: Some((LARGE_PNG_200X200.len() as f64 * 0.9).floor() as usize),
                jpeg_quality: None,
            }),
        )
        .await
        .unwrap();

        let result_buffer = pi_js::b64::decode(&result.data);
        assert!(result_buffer.len() < original_size);
        assert!(result.data.len() < LARGE_PNG_200X200.len());
    }

    #[tokio::test]
    async fn should_return_null_when_image_cannot_be_resized_below_max_bytes() {
        let result = resize_image(
            &image_bytes(LARGE_PNG_200X200),
            "image/png",
            Some(ImageResizeOptions {
                max_width: Some(2000),
                max_height: Some(2000),
                max_bytes: Some(1),
                jpeg_quality: None,
            }),
        )
        .await;

        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn should_pick_jpeg_over_a_larger_png_when_resizing_a_photo() {
        let _photon = load_photon().await.expect("Photon available");
        let width: u32 = 1200;
        let height: u32 = 800;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let mut seed: u32 = 1;
        for y in 0..height {
            for x in 0..width {
                seed = (seed.wrapping_mul(1103515245).wrapping_add(12345)) & 0x7fff_ffff;
                let noise = (seed % 6) as u8;
                let i = ((y * width + x) * 4) as usize;
                pixels[i] = (((x as f64 / width as f64) * 200.0) as u8).saturating_add(noise);
                pixels[i + 1] = (((y as f64 / height as f64) * 200.0) as u8).saturating_add(noise);
                pixels[i + 2] = ((128.0 + ((x as f64) / 30.0).sin() * 60.0) as u8).saturating_add(noise);
                pixels[i + 3] = 255;
            }
        }
        let image = PhotonImage::new(pixels, width, height).expect("new image");
        let input = image.get_bytes_jpeg(85).expect("jpeg bytes");

        let result = resize_image_in_process(
            &input,
            "image/jpeg",
            Some(ImageResizeOptions {
                max_width: Some(1000),
                max_height: Some(1000),
                max_bytes: None,
                jpeg_quality: None,
            }),
        )
        .await
        .expect("resized");

        assert!(result.was_resized);
        assert_eq!(result.mime_type, "image/jpeg");
        assert_eq!(result.width, 1000);
        assert!(result.data.len() < input.len().div_ceil(3) * 4 * 2);
    }

    #[tokio::test]
    async fn should_handle_jpeg_input() {
        let result = resize_image(
            &image_bytes(TINY_JPEG),
            "image/jpeg",
            Some(ImageResizeOptions {
                max_width: Some(100),
                max_height: Some(100),
                max_bytes: Some(1024 * 1024),
                jpeg_quality: None,
            }),
        )
        .await
        .unwrap();

        assert_eq!(result.was_resized, false);
        assert_eq!(result.original_width, 2);
        assert_eq!(result.original_height, 2);
    }
}

mod format_dimension_note_tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn should_return_undefined_for_non_resized_images() {
        let note = format_dimension_note(&ResizedImage {
            data: String::new(),
            mime_type: "image/png".to_owned(),
            original_width: 100,
            original_height: 100,
            width: 100,
            height: 100,
            was_resized: false,
        });
        assert_eq!(note, None);
    }

    #[test]
    fn should_return_formatted_note_for_resized_images() {
        let note = format_dimension_note(&ResizedImage {
            data: String::new(),
            mime_type: "image/png".to_owned(),
            original_width: 2000,
            original_height: 1000,
            width: 1000,
            height: 500,
            was_resized: true,
        })
        .unwrap();
        assert!(note.contains("original 2000x1000"));
        assert!(note.contains("displayed at 1000x500"));
        assert!(note.contains("2.00"));
    }
}
