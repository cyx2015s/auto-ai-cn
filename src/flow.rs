//! 翻译流程
//! 获取自上从运行以来更新的所有 mod
//! 对于每个 mod
//!   获取 mod 的所有翻译文件
//!   如果本地有缓存，则顺便获取差异
//!   将预先构筑好的提示词送入 llm
//!     （提示词包含：原版游戏的中英文对照，上次的全部翻译文件，当前翻译任务）
//!     （如果本地有缓存，翻译任务只包含差异部分）
//!   将 llm 的输出与本地缓存进行合并，生成新的翻译
//!   保存（通过 Persistent<T, S>）新的翻译文件到本地缓存
//!
//!   🤔：可能无法妥善处理文件重命名的情况。
//!   约定：通过函数调用 function calling 让 llm 提交翻译

#[cfg(test)]
mod tests {
    use std::env::*;

    use deepseek_api::{
        CompletionsRequestBuilder, DeepSeekClientBuilder, RequestBuilder,
        request::{Function, MessageRequest, ToolMessageRequest, ToolObject, ToolType},
        response::FinishReason,
    };

    #[tokio::test]
    async fn main() -> anyhow::Result<()> {
        dotenvy::dotenv().ok();
        let client = DeepSeekClientBuilder::new(var("DEEPSEEK_KEY")?).build()?;
        let parameters = serde_json::from_str(
            r#"{
            "type": "object",
            "properties": {
                "input": {
                    "type": "number",
                    "description": "The input to the function"
                }
            }
    }"#,
        )?;

        let tool_object = ToolObject {
            tool_type: ToolType::Function,
            function: Function {
                name: "test_function".to_string(),
                description: "A simple test function".to_string(),
                parameters,
            },
        };

        let tool_objects: Vec<ToolObject> = vec![tool_object];
        let mut messages = vec![MessageRequest::user(
            "Call the function with parameter to test the tool calling feature.",
        )];
        let resp = CompletionsRequestBuilder::new(&messages)
            .tools(&tool_objects)
            .do_request(&client)
            .await?
            .must_response();
        let mut id = String::new();
        let mut arguments = String::new();
        if resp.choices[0].finish_reason == FinishReason::ToolCalls {
            if let Some(msg) = &resp.choices[0].message {
                if let Some(tool) = &msg.tool_calls {
                    id = tool[0].id.clone();
                    println!("Function id: {}", id);
                    println!("Function name: {}", tool[0].function.name);
                    println!("Function parameters: {:?}", tool[0].function.arguments);
                    arguments = tool[0].function.arguments.clone();
                }
                messages.push(MessageRequest::Assistant(msg.clone()));
            }
        }

        messages.push(MessageRequest::Tool(ToolMessageRequest::new(
            &format!("Called test_function with arguments: {}", arguments),
            &id,
        )));
        let resp = CompletionsRequestBuilder::new(&messages)
            .tools(&tool_objects)
            .do_request(&client)
            .await?
            .must_response();
        println!(
            "Reply with my function: {:?}",
            resp.choices[0].message.as_ref().unwrap().content
        );
        dbg!(messages);
        Ok(())
    }
}
