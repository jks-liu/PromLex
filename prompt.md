# Prompt List

## 2026

### 8/5

- 打开软件后有个黑框，请去除
- 翻译语言除了列表中的语言，还应允许用户自定义
- 翻译word文档时，会有以下错误：missing field `text` at line 1 column 150
- drop file 仅在网页版有效，desptop版无效
- source / target language加一个switch按钮
- 加一个历史记录功能
- 打开后无任何操作也会占用很高的CPU，分别是：
    * webView2管理器
    * webView2：tranova
    * webView2实用工具：network service

### 8/6

- 文件翻译后点击下载没有反应
- 翻译后的文件支持两种形式：
    1. 仅包含译文
    2. 原文译文混合
- 文件翻译时每次只翻译一段，效率太低，一次翻译多段（具体多少在设置里设置token数（或近似字符数）上限），提高效率
- 调用AI可以并发，并发数也在设置里设置
- 文件翻译显示进度条详情
- 翻译文件的同时允许进入其它tab，回来进度不会丢失；
期间可以进行其它文本翻译，且文本翻译优先级更高（注意处理好AI调用的并发）
- 修复bug：source和target language在离开当前tab再进来时就丢失了


### 8/7
- 文件翻译失败了，ollama和其openai兼容接口失败信息如下
Network request failed: error sending request for url (http://127.0.0.1:11434/api/chat)
Network request failed: error sending request for url (http://127.0.0.1:11434/v1/chat/completions)
但是我在ollama日志已经看到请求了，说明网络本身没问题

- version:bump 加一个显示当前版本号的功能
- 15分钟超时有点太过了，这个时间让用户设置吧，默认180秒
- 你在测试时可以使用本地ollama（不要使用ollama命令，而是curl或者直接rust代码）

- 完善文件翻译后下载按钮（因网页版下载被浏览器接管仅desktop版）
    * 下载中/后显示提示，并显示两个按钮：“打开文件”， “打开文件夹”
    * 设置中添加下载位置设置
        - 原始文件所在文件夹
        - 系统下载文件夹
        - 自定义文件夹
    * 设置中添加如下选项：文件翻译后自动下载
    * 设置中添加如下选项：下载时询问下载位置（默认上一次位置）

- 默认使用流式输出，这样
    1. 大文件不易超时
    2. 能更精确的显示翻译任务百分比
    3. 翻译时能实时显示翻译结果
- 除了字符上上限，设置里再加一个segments上限（默认100），以防复杂的json格式
- 每个batch失败后重试，默认重试5次（设置里可设置），batch翻译失败不导致文件翻译失败，而是翻译后的文件相关部分不翻译，并在UI中提示用户部分文本未翻译，并添加一个重试按钮，此按钮仅重试失败的batch。
- 文件翻译的并发上限设置为max(1, 设置的上限-1)，这样文件翻译就不会阻塞文本翻译


### 8/10

根据tmp-goal.md完善程序
- 修复：Translate页正在进行的翻译任务在切换到其它页再回来就不见了。
- 修复：Translate页不是实时，改成流式，实时显示翻译结果
- 添加一个日志页面，默认不显示，可在设置里开启，并且可以设置级别，默认级别是info。请在程序的关键地方加上日志。日志页面包含两个部分：
    * 系统日志
    * 与AI的对话内容：包括时间，调用的模型，发给AI的内容，AI返回的内容，其它必要信息等等
- 由于Ollama等AI都支持openai兼容接口，因此AI provider仅支持openai兼容接口（openai responses api）就行。但可以提示用户如何填入ollama等服务的openai兼容接口地址
- 如果可以的话，自动获取模型列表，也允许用户输入模型。如果可以的话自动获取模型的上下文大小，也可以用户自行输入。
- 删除字符上限设置项，默认为上下文大小除以4，这个值是和模型绑定的
- segments上限也从设置中移到ai provider中和模型绑定，默认值为16
- 修改重试逻辑：如果某个batch由于AI返回结果解析的原因失败，将这个batch拆分成两个batch，直至只剩一个segment回退到非batch模式
- 并发设置也从设置中移到ai provider中和模型绑定，默认值为2
- ai provider设置模型时添加一个“文本翻译常用模型”选项，默认disable，enable时文件翻译使用这个模型时并发数-1（除非并发数就是1）保留给文本翻译使用，disable时按照设置的并发数并发
- 当segment总数小于等于一个batch的segments上限时，也应该等分成多个（并发数）batch。或者说是：“segment总数”<=“一个batch的segments上限”x“并发数”时
- 文本翻译和文件翻译页添加一个推理程度的下拉菜单，控制ai模型的思考强度
- 文件翻译页面加一个翻译前总结的checkbox，默认enable。enable时截取翻译文件最多达模型上下文大小除以2的文本数量先给AI做一个summary，这个总结会在后续的翻译中附在提示词中，力求让ai翻译时充分理解上下文，提高专有名词的翻译精度。
- history页面中的长文本应该auto wrap
- 文件翻译时间较长，添加一个取消按钮
- 修复文件翻译顺序问题：一个任务还没好，另一个任务加进来应该等待
- 修复：文件翻译完成时，打开文件和打开文件夹按钮点击会出错：Not allowed to open path
- 修改AI返回格式：当前使用json格式，但此格式会提高解析失败的概率，改成如下格式
    * 选取一个合理的分隔符，单独一行，用于分割segments；AI返回的结果也让其用这个分隔符分隔。
    * 如果某个segment包含这个分隔符，则此segment的翻译回退到简单文本的非batch模式
    * 由于只是简单的分隔，使用流式输出后，就能简单地知道当前batch已经翻译了几个segment，从而更好地显示文件翻译进度
- 文件翻译添加pdf支持
- 翻译完excel后，打开excel文件会提示需要修复，修复完后可以正常打开，修复记录如下
```
<recoveryLog xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<logFileName>error125080_02.xml</logFileName>
<summary>在文件“C:\p\xxx-translated.xlsx”中检测到错误</summary>
<repairedRecords>
<repairedRecord>已修复的记录: /xl/sharedStrings.xml 部分的 字符串属性 (字符串)</repairedRecord>
</repairedRecords>
</recoveryLog>
```
- 分析代码中哪些是在重复造轮子，对于代码量比较大的轮子，请使用成熟的库替代，仅限于成熟的库，不要使用那种根本没人维护的小库。对于只有几十行代码的小轮子请保留

### 8/12
- 默认推理强度改成None
- 失败的文件翻译添加一个重试按钮
- 文件翻译时控制ai返回的summary长度，这个summary只要能提示翻译主题就好，无需完整总结。summary时发送给ai的文本长度除了原有限制外再加一个4K的限制。
- AI provider的模型设置中添加一个关于proxy的菜单，有三个选项
    1. 不使用代理
    2. 使用设置中的设置的代理
    3. 使用系统代理
- “文本翻译常用模型”英文翻译改成“Commonly used models for text translation”，并加小字解释这个选项的含义
- UI中的很多checkbox不管有没有enable都显示“enabled”提示文本，请删除这个提示文本
- 左侧的tab不应该跟着右边的内容一起scroll
- 另一台机器翻译文件会显示如下错误（本机没问题）
    * Unable to summarize document: Selected AI provider does not exist or is disabled
    * Selected AI provider was not found
  并且只在使用桌面版时有问题，使用web访问就没问题
- AI request timeout设置中的提示文字“including local model generation”是什么意思？
- history中source文本重复显示了

### 8/13

API测试正常但翻译文件显示如下错误
The file translation failed before a result was created.
Unable to summarize document: Responses API returned no output text

- 开启文件summary还是有如下问题
The file translation failed before a result was created.
Unable to summarize document: Responses API returned no output text
我猜测可能是有些模型无法关闭推理
我的建议是不使用“max_output_tokens”硬控，而是通过提示词。如果返回summary还是过长可以适当截取。

### 8/28

- 修复：pdf文件翻译完成后是txt文件而不是pdf。请确保文件翻译完还是同类型文件
- 语言列表中的选项应该以选项所指的语言显示
- 语言列表不需要根据已输入内容filter选项
- 修复：AI providers设置中有一个悬空的checkbox无任何文字

- 当AI是流式输出时，每当接收到消息时超时时间应该重置

### 9/2

- 我使用qwen3.8-27b(vllm with qwen3 reasoning parser)，当前推理开关/强度控制有问题。推理控制好像没有通用方法，但请支持主流模型，比如像vllm一样有一个reasoning parser的选项让用户选择、或根据模型名自动适配。

### 9/21

- Logs中的System logs和AI conversations分成两个tab显示，且应该有分页。并增加清空、清空某日期之前、删除某条的功能。
- 翻译Prompt（translate和Files）除了可以选择预设，新增在预设prompt的基础上添加额外Prompt文字的功能。

### 10/9

- 我改了文件夹名字就build失败了，这是个问题，请修复
- 我决定将软件名由`Tranova`改成`PromLex`，请帮我实施
- 添加一个中文名叫`语锚`，软件图标改成一只可爱的小猫站在羽毛上，羽毛上写着汉字`译`

### 10/14

- pdf文件翻译的输出排版和输入排版相差很大，请改善（也许可以使用更成熟的pdf库？，我不知道有没有），不要怕麻烦就偷工减料。

## TODO
- 取消后再翻译可能有问题




