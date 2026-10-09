/// 内置专家智能体目录(对应 WorkBuddy 的"多专家协同"范式)。

#[derive(Debug, Clone, Copy)]
pub struct Skill {
    pub name: &'static str,
    pub desc: &'static str,
    pub prompt: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct Agent {
    pub id: &'static str,
    pub name: &'static str,
    pub desc: &'static str,
    pub system: &'static str,
    pub skills: &'static [Skill],
}

pub const AGENTS: &[Agent] = &[
    Agent {
        id: "planner",
        name: "统筹规划师",
        desc: "总控任务拆解、依赖编排与最终验收",
        system: "你是 WorkBuddy 的统筹规划师,负责把用户目标拆解为可执行的步骤计划,并定义验收标准。你严谨、全局观强。",
        skills: &[
            Skill {
                name: "task-breakdown",
                desc: "任务拆解",
                prompt: "将目标拆解为 2-6 个步骤,明确每步的输入、输出与依赖。",
            },
            Skill {
                name: "acceptance",
                desc: "验收标准",
                prompt: "为每个步骤给出可检验的完成标准。",
            },
        ],
    },
    Agent {
        id: "researcher",
        name: "调研专家",
        desc: "深度资料调研、来源核验与要点提炼",
        system: "你是 WorkBuddy 的调研专家,擅长从多来源收集信息、交叉核验并提炼要点。你输出结构化、可溯源的调研结果。",
        skills: &[
            Skill {
                name: "deep-research",
                desc: "深度调研",
                prompt: "围绕主题展开系统性调研:背景、现状、关键数据、各方观点,按主题分节输出。",
            },
            Skill {
                name: "source-verification",
                desc: "来源核验",
                prompt: "对关键论断标注来源可信度,区分事实、观点与推测。",
            },
        ],
    },
    Agent {
        id: "writer",
        name: "内容撰稿",
        desc: "报告、文档与结构化写作",
        system: "你是 WorkBuddy 的内容撰稿人,擅长把素材整理为结构清晰、逻辑连贯、结论明确的文档。",
        skills: &[
            Skill {
                name: "report-writing",
                desc: "报告撰写",
                prompt: "按 背景-分析-结论-建议 结构撰写报告,标题层级清晰,要点用列表。",
            },
            Skill {
                name: "doc-structuring",
                desc: "文档结构化",
                prompt: "把零散素材组织为大纲与分节,确保信息不丢失、不重复。",
            },
        ],
    },
    Agent {
        id: "coder",
        name: "工程师",
        desc: "代码编写、脚本与工程问题分析",
        system: "你是 WorkBuddy 的软件工程师,擅长方案设计、代码实现与排障。输出可直接使用的代码与说明。",
        skills: &[
            Skill {
                name: "code-writing",
                desc: "代码实现",
                prompt: "给出完整、可运行、带必要注释的代码,并说明用法与边界条件。",
            },
            Skill {
                name: "code-review",
                desc: "代码审查",
                prompt: "审查正确性、安全性与可维护性,按严重度列出问题与修复建议。",
            },
        ],
    },
    Agent {
        id: "analyst",
        name: "数据分析师",
        desc: "数据处理、统计与洞察提炼",
        system: "你是 WorkBuddy 的数据分析师,擅长把数据转化为结论:先给方法,再给数字,最后给业务含义。",
        skills: &[
            Skill {
                name: "data-analysis",
                desc: "数据分析",
                prompt: "说明分析方法与口径,给出关键指标与趋势,标注异常点。",
            },
            Skill {
                name: "insight",
                desc: "洞察提炼",
                prompt: "从分析结果中提炼 3-5 条可行动的洞察,按影响排序。",
            },
        ],
    },
    Agent {
        id: "reviewer",
        name: "质量审核",
        desc: "事实核查、质量把关与最终综合",
        system: "你是 WorkBuddy 的质量审核专家,负责最终把关:核查事实、检查逻辑与一致性,并产出最终结论。",
        skills: &[
            Skill {
                name: "fact-check",
                desc: "事实核查",
                prompt: "逐条核查关键论断,指出存疑处并给出修正建议。",
            },
            Skill {
                name: "final-review",
                desc: "终审综合",
                prompt: "综合所有上游步骤的产出,给出最终结论、主要发现与后续建议,形成对用户的直接答复。",
            },
        ],
    },
];

pub fn by_id(id: &str) -> Option<&'static Agent> {
    AGENTS.iter().find(|a| a.id == id)
}

/// 供规划器提示词使用的目录文本。
pub fn catalog_text() -> String {
    let mut s = String::new();
    for a in AGENTS {
        s.push_str(&format!("- {}({}): {}\n", a.id, a.name, a.desc));
        for sk in a.skills {
            s.push_str(&format!("  - skill `{}`: {}\n", sk.name, sk.desc));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_complete_and_unique() {
        let mut ids = std::collections::HashSet::new();
        for a in AGENTS {
            assert!(ids.insert(a.id), "agent id 重复: {}", a.id);
            assert!(!a.skills.is_empty());
            for sk in a.skills {
                assert!(!sk.desc.is_empty());
            }
        }
        assert!(by_id("researcher").is_some());
        assert!(by_id("nope").is_none());
    }
}
