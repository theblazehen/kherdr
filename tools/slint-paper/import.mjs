#!/usr/bin/env bun
/** Import resolved Slint scenes, not screenshots. Writes only new, explicitly targeted artboards. */
import { readFile, writeFile, mkdir } from "node:fs/promises";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";

const escape = (value) =>
    String(value)
        .replaceAll("&", "&amp;")
        .replaceAll("<", "&lt;")
        .replaceAll(">", "&gt;")
        .replaceAll('"', "&quot;");
const px = (value) => `${Math.round(value * 1000) / 1000}px`;
// Match the explicit fallback order registered in src/fonts.rs.
const fallbackFamilies = ["TBGothic", "Code2000", "Amazon Ember"];

export function prepare(scene) {
    if (scene.schema !== 1 || scene.unsupported.length)
        throw new Error(
            `Scene ${scene.name} cannot be imported: ${JSON.stringify(scene.unsupported)}`,
        );
    const children = new Map();
    const families = new Set();
    for (const node of scene.nodes) {
        const list = children.get(node.parent) ?? [];
        list.push(node);
        children.set(node.parent, list);
    }
    function html(node, includeChildren = true) {
        const [x, y, width, height] = node.rect;
        if (![x, y, width, height].every(Number.isFinite))
            throw new Error(`Non-finite geometry at ${node.name}`);
        const css = {
            ...node.styles,
            position: "absolute",
            left: px(x),
            top: px(y),
            width: px(Math.max(0, width)),
            height: px(Math.max(0, height)),
            "box-sizing": "border-box",
            "flex-shrink": "0",
        };
        let inner = includeChildren
            ? (children.get(node.id) ?? [])
                  .map((child) => html(child))
                  .join("\n")
            : "";
        if (node.kind === "text") {
            if (width <= 0 || height <= 0) return "";
            if (!node.text && !inner) return "";
            const family = css["font-family"];
            if (!family) throw new Error(`Unresolved font at ${node.name}`);
            const stack = [...new Set([family, ...fallbackFamilies])];
            for (const name of stack) families.add(name);
            css["font-family"] = stack
                .map((name) => `'${name.replaceAll("'", "")}'`)
                .join(",");
            const alignment = css["vertical-align"];
            delete css["vertical-align"];
            css.display = "flex";
            css["flex-direction"] = "column";
            css["justify-content"] = {
                top: "flex-start",
                center: "center",
                bottom: "flex-end",
            }[alignment];
            if (!css["justify-content"])
                throw new Error(`Unknown vertical alignment: ${alignment}`);
            css.overflow = "hidden";
            inner =
                `<div layer-name="Text" style="width:${px(width)};min-width:${px(width)};max-width:${px(width)};flex-shrink:0;white-space:${css["white-space"]};text-align:${css["text-align"]};text-overflow:${css["text-overflow"]};overflow:hidden">${escape(node.text ?? "")}</div>` +
                inner;
        }
        if (
            node.kind === "rectangle" &&
            Number.parseFloat(css["border-width"]) > 0
        ) {
            css["border-style"] = "solid";
            if (inner) {
                // Slint children start at the outer edge; CSS borders otherwise shift that origin.
                const paint = {
                    position: "absolute",
                    left: "0px",
                    top: "0px",
                    width: "100%",
                    height: "100%",
                    "box-sizing": "border-box",
                };
                for (const key of [
                    "background",
                    "border-color",
                    "border-width",
                    "border-radius",
                    "border-style",
                ]) {
                    if (css[key] !== undefined) {
                        paint[key] = css[key];
                        delete css[key];
                    }
                }
                inner =
                    `<div layer-name="Border and fill" style="${escape(
                        Object.entries(paint)
                            .map(([key, value]) => `${key}:${value}`)
                            .join(";"),
                    )}"></div>` + inner;
            }
        }
        const style = Object.entries(css)
            .map(([key, value]) => `${key}:${value}`)
            .join(";");
        if (node.kind === "image") {
            if (!node.text?.startsWith("data:image/png;base64,"))
                throw new Error(`Unresolved native image at ${node.name}`);
            return `<img layer-name="${escape(node.name)} [s${node.id}]" src="${escape(node.text)}" style="${escape(style)}"/>${inner}`;
        }
        if (node.kind === "group" && !inner) return "";
        return `<div layer-name="${escape(node.name)} [s${node.id}]" style="${escape(style)}">${inner}</div>`;
    }
    const roots = [];
    function collect(node, origin = [0, 0]) {
        const rect = [
            node.rect[0] + origin[0],
            node.rect[1] + origin[1],
            node.rect[2],
            node.rect[3],
        ];
        if (node.kind === "window") {
            roots.push({
                node,
                html: html({ ...node, kind: "rectangle", rect }, false),
            });
            for (const child of children.get(node.id) ?? [])
                collect(child, rect);
        } else if (
            node.kind === "group" &&
            Object.keys(node.styles).length === 0
        ) {
            for (const child of children.get(node.id) ?? [])
                collect(child, rect);
        } else {
            roots.push({ node, html: html({ ...node, rect }) });
        }
    }
    for (const node of children.get(null) ?? []) collect(node);
    return { roots, families: [...families] };
}

export class Paper {
    constructor(url = "http://127.0.0.1:29979/mcp") {
        this.url = url;
        this.id = 0;
        this.headers = {
            "Content-Type": "application/json",
            Accept: "application/json, text/event-stream",
        };
    }
    async rpc(method, params) {
        const id = ++this.id;
        const response = await fetch(this.url, {
            method: "POST",
            headers: this.headers,
            body: JSON.stringify({ jsonrpc: "2.0", id, method, params }),
            signal: AbortSignal.timeout(90000),
        });
        if (!response.ok)
            throw new Error(
                `Paper HTTP ${response.status}: ${await response.text()}`,
            );
        const session = response.headers.get("mcp-session-id");
        if (session) this.headers["Mcp-Session-Id"] = session;
        const raw = await response.text();
        const message = response.headers
            .get("content-type")
            ?.includes("text/event-stream")
            ? raw
                  .split("\n")
                  .filter((line) => line.startsWith("data: "))
                  .map((line) => JSON.parse(line.slice(6)))
                  .find((item) => item.id === id)
            : JSON.parse(raw);
        if (!message || message.error)
            throw new Error(
                `Paper RPC: ${JSON.stringify(message?.error ?? raw)}`,
            );
        return message.result;
    }
    async connect() {
        const result = await this.rpc("initialize", {
            protocolVersion: "2025-03-26",
            capabilities: {},
            clientInfo: { name: "kherdr-slint-paper", version: "0.1.0" },
        });
        this.headers["MCP-Protocol-Version"] = result.protocolVersion;
        const response = await fetch(this.url, {
            method: "POST",
            headers: this.headers,
            body: JSON.stringify({
                jsonrpc: "2.0",
                method: "notifications/initialized",
            }),
            signal: AbortSignal.timeout(10000),
        });
        if (!response.ok)
            throw new Error(
                `Paper initialization notification: HTTP ${response.status}`,
            );
    }
    async call(name, args = {}) {
        const result = await this.rpc("tools/call", { name, arguments: args });
        if (result.isError)
            throw new Error(`Paper ${name}: ${JSON.stringify(result.content)}`);
        const text = result.content
            ?.filter((item) => item.type === "text")
            .map((item) => item.text)
            .join("\n");
        let data;
        try {
            data = JSON.parse(text);
        } catch {
            data = { text };
        }
        if (data.errors?.length)
            throw new Error(`Paper ${name}: ${data.errors.join("; ")}`);
        return { data, content: result.content };
    }
}

async function main() {
    const args = process.argv.slice(2);
    const directory = args.shift();
    let fileId, state;
    while (args.length) {
        const flag = args.shift();
        if (flag === "--file") fileId = args.shift();
        else if (flag === "--state") state = args.shift();
        else throw new Error(`Unknown flag ${flag}`);
    }
    if (!directory || !fileId)
        throw new Error(
            "Usage: bun tools/slint-paper/import.mjs SCENES --file PAPER_FILE_ID [--state STATE]",
        );
    const manifest = JSON.parse(
        await readFile(join(directory, "manifest.json"), "utf8"),
    );
    const scenes = await Promise.all(
        manifest
            .filter((item) => !state || item.state === state)
            .map(async (item) =>
                JSON.parse(
                    await readFile(
                        join(directory, `${item.state}.json`),
                        "utf8",
                    ),
                ),
            ),
    );
    if (!scenes.length) throw new Error("No matching scenes");
    const prepared = scenes.map((scene) => ({ scene, ...prepare(scene) }));
    const paper = new Paper();
    await paper.connect();
    await paper.call("get_guide", { topic: "paper-mcp-instructions" });
    await paper.call("get_basic_info", { fileId });
    await paper.call("get_font_family_info", {
        familyNames: [...new Set(prepared.flatMap((item) => item.families))],
    });
    const receipts = [];
    const receiptDirectory = join(directory, `paper-import-${Date.now()}`);
    await mkdir(receiptDirectory, { recursive: true });
    console.log(`Receipts and native Paper previews: ${receiptDirectory}`);
    try {
        for (const item of prepared) {
            const { scene } = item;
            const board = await paper.call("create_artboard", {
                fileId,
                name: `Current UI · ${scene.name}`,
                styles: {
                    width: `${scene.width}px`,
                    height: `${scene.height}px`,
                    backgroundColor: "#ffffff",
                    overflow: "hidden",
                },
            });
            const artboardId = board.data.id;
            if (!artboardId)
                throw new Error(
                    `Unexpected artboard response: ${JSON.stringify(board.data)}`,
                );
            const receipt = {
                state: scene.name,
                artboardId,
                sourceNodes: scene.nodes.length,
                complete: false,
                groups: [],
            };
            receipts.push(receipt);
            await writeFile(
                join(receiptDirectory, "receipt.json"),
                JSON.stringify({ fileId, receipts }, null, 2),
            );
            for (const root of item.roots) {
                if (!root.html) continue;
                const result = await paper.call("write_html", {
                    fileId,
                    targetNodeId: artboardId,
                    mode: "insert-children",
                    html: root.html,
                });
                receipt.groups.push(result.data);
            }
            receipt.complete = true;
            const image = await paper.call("get_screenshot", {
                fileId,
                nodeId: artboardId,
                scale: 1,
            });
            for (const part of image.content ?? [])
                if (part.type === "image") {
                    const extension = {
                        "image/jpeg": "jpg",
                        "image/png": "png",
                    }[part.mimeType];
                    if (!extension)
                        throw new Error(
                            `Unsupported screenshot format: ${part.mimeType}`,
                        );
                    await writeFile(
                        join(receiptDirectory, `${scene.name}.${extension}`),
                        Buffer.from(part.data, "base64"),
                    );
                }
            await writeFile(
                join(receiptDirectory, "receipt.json"),
                JSON.stringify({ fileId, receipts }, null, 2),
            );
            console.log(
                `Imported ${scene.name}: ${scene.nodes.length} resolved source items`,
            );
        }
    } finally {
        await writeFile(
            join(receiptDirectory, "receipt.json"),
            JSON.stringify({ fileId, receipts }, null, 2),
        );
        await paper.call("finish_working_on_nodes", {
            fileId,
            nodeIds: receipts.map((item) => item.artboardId),
        });
    }
}
if (
    process.argv[1] &&
    import.meta.url === pathToFileURL(resolve(process.argv[1])).href
)
    main().catch((error) => {
        console.error(error);
        process.exitCode = 1;
    });
