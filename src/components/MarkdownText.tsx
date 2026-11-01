import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";

export default function MarkdownText({ text }: { text: string }) {
  return <Markdown remarkPlugins={[remarkGfm]} skipHtml>{text}</Markdown>;
}
