use std::cmp::Ordering;

pub async fn ollama_embedding(text: &str) -> Result<Vec<f32>, String> {
    let base = std::env::var("KAREN_OLLAMA_URL")
        .unwrap_or_else(|_| "http://10.50.101.11:11434/v1/chat/completions".into())
        .replace("/v1/chat/completions", "/api/embeddings");
    let model =
        std::env::var("KAREN_OLLAMA_EMBED_MODEL").unwrap_or_else(|_| "nomic-embed-text".into());
    let response = reqwest::Client::new()
        .post(base)
        .json(&serde_json::json!({"model": model, "prompt": text}))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("embedding endpoint returned {}", response.status()));
    }
    let body: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    body.get("embedding")
        .cloned()
        .ok_or_else(|| "embedding missing from Ollama response".into())
        .and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string()))
}

/// Cosine similarity for normalized or unnormalized embedding vectors.
pub fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    if left.is_empty() || left.len() != right.len() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut left_norm = 0.0;
    let mut right_norm = 0.0;
    for (&a, &b) in left.iter().zip(right) {
        dot += a * b;
        left_norm += a * a;
        right_norm += b * b;
    }
    let denominator = left_norm.sqrt() * right_norm.sqrt();
    if denominator == 0.0 {
        0.0
    } else {
        dot / denominator
    }
}

pub fn rank<T: Clone>(query: &[f32], candidates: &[(T, Vec<f32>)], limit: usize) -> Vec<(T, f32)> {
    let mut ranked: Vec<_> = candidates
        .iter()
        .map(|(value, vector)| (value.clone(), cosine_similarity(query, vector)))
        .collect();
    ranked.sort_by(|left, right| right.1.partial_cmp(&left.1).unwrap_or(Ordering::Equal));
    ranked.truncate(limit);
    ranked
}

#[cfg(test)]
mod tests {
    use super::{cosine_similarity, rank};

    #[test]
    fn compares_direction_not_magnitude() {
        assert!((cosine_similarity(&[1.0, 0.0], &[10.0, 0.0]) - 1.0).abs() < 0.0001);
        assert!((cosine_similarity(&[1.0, 0.0], &[0.0, 1.0])).abs() < 0.0001);
    }

    #[test]
    fn ranks_closest_candidates_first() {
        let result = rank(
            &[1.0, 0.0],
            &[("far", vec![0.0, 1.0]), ("close", vec![2.0, 0.0])],
            1,
        );
        assert_eq!(result[0].0, "close");
    }
}
