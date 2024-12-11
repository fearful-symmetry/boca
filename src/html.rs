use minijinja::Environment;

use crate::Cli;

pub fn generate(config: Cli) -> anyhow::Result<String> {
    let raw = r#"
    <!doctype html>
    <html lang="en">
        <head>
            <script src="https://unpkg.com/htmx.org@2.0.3"></script>
            <script src="https://unpkg.com/htmx-ext-sse@2.2.2/sse.js"></script>

                <style>
                    :root {
                        color-scheme: {% if dark %} dark {% else %} light {% endif %};
                    }
                    body {
                        display: flex;
                        align-items: center;
                        justify-content: center;
                    }
                    .text-body {
                        max-width: 40%;
                    }
                    pre {
                        page-break-inside: avoid;
                        font-family: monospace;
                        font-size: 15px;
                        line-height: 1.6;
                        margin-bottom: 1.6em;
                        max-width: 100%;
                        overflow: auto;
                        padding: 1em 1.5em;
                        display: block;
                        word-wrap: break-word;
                        background: light-dark(#EDEDED, #686868);
                        border-left: 8px solid #f36d33;
                    }
                    blockquote {
                        margin:10px auto;
                        font-style:italic;
                        padding:1.0em 30px 1.2em 75px;
                        border-left:8px solid #78C0A8 ;
                        line-height:1.6;
                        position: relative;
                        background: light-dark(#EDEDED, #686868);
                    }
                </style>

                {% if stylesheet %}<link href="{{stylesheet}}" rel="stylesheet"/>{% endif %}
        </head>
        <body>
            <div class="text-body">
                <span id="data-value" hx-ext="sse" sse-connect="/sse/{{filename}}" sse-swap="body" >
                
                Loading...</span>
            </div>
        </body>
    </html>
    "#;
    let mut env = Environment::new();
    env.add_template("root", raw)?;
    let tmpl = env.get_template("root")?;
    let rendered = tmpl.render(config)?;
    Ok(rendered)
}