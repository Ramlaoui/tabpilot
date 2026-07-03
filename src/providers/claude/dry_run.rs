pub(super) fn dry_run_response(prompt: &str) -> String {
    let insert_text = if prompt.contains("language_id: lua") {
        " -- claude"
    } else if prompt.contains("language_id: python")
        || prompt.contains("language_id: sh")
        || prompt.contains("language_id: bash")
        || prompt.contains("language_id: zsh")
    {
        " # claude"
    } else {
        " // claude"
    };

    serde_json::json!({
        "suggestions": [
            {
                "insert_text": insert_text,
                "score": 1.0
            }
        ]
    })
    .to_string()
}

pub(super) fn dry_run_answer_response(question: &str) -> String {
    if question.to_ascii_lowercase().contains("matplotlib") {
        "import matplotlib.pyplot as plt".to_owned()
    } else {
        format!("Answer: {question}")
    }
}
