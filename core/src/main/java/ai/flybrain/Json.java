package ai.flybrain;

import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * A small JSON reader and writer.
 *
 * <p>The service has exactly two JSON jobs: read a pack's metadata block at
 * startup, and write a state frame per tick. Both are small and shaped by us.
 * A dependency would buy features neither job needs, so this is here instead --
 * about two hundred lines against a library and its transitive tree.
 *
 * <p>It is not a general-purpose parser. It reads what the extractor writes.
 */
public final class Json {

    private Json() {}

    // -- model ---------------------------------------------------------------

    public static sealed interface Value permits Obj, Arr, Str, Num, Bool, Null {
        default Obj asObj() { throw new IllegalStateException("not an object: " + this); }
        default List<Value> asArray() { throw new IllegalStateException("not an array: " + this); }
        default String asString() { throw new IllegalStateException("not a string: " + this); }
        default double asDouble() { throw new IllegalStateException("not a number: " + this); }
    }

    public record Obj(Map<String, Value> fields) implements Value {
        @Override public Obj asObj() { return this; }
        public Value get(String k) { return fields.get(k); }
        public Iterable<Map.Entry<String, Value>> entries() { return fields.entrySet(); }
        public String str(String k) {
            Value v = fields.get(k);
            return v == null || v instanceof Null ? null : v.asString();
        }
        public double num(String k) {
            Value v = fields.get(k);
            if (v == null) throw new IllegalStateException("missing field: " + k);
            return v.asDouble();
        }
    }

    public record Arr(List<Value> items) implements Value {
        @Override public List<Value> asArray() { return items; }
    }

    public record Str(String value) implements Value {
        @Override public String asString() { return value; }
    }

    public record Num(double value) implements Value {
        @Override public double asDouble() { return value; }
    }

    public record Bool(boolean value) implements Value {}

    public record Null() implements Value {}

    // -- parsing -------------------------------------------------------------

    public static Value parse(String src) {
        Parser p = new Parser(src);
        p.skipWs();
        Value v = p.value();
        p.skipWs();
        if (p.pos < src.length()) {
            throw new IllegalArgumentException("trailing content at offset " + p.pos);
        }
        return v;
    }

    private static final class Parser {
        private final String s;
        private int pos;

        Parser(String s) { this.s = s; }

        void skipWs() {
            while (pos < s.length() && Character.isWhitespace(s.charAt(pos))) pos++;
        }

        Value value() {
            if (pos >= s.length()) throw err("unexpected end of input");
            char c = s.charAt(pos);
            return switch (c) {
                case '{' -> object();
                case '[' -> array();
                case '"' -> new Str(string());
                case 't' -> literal("true", new Bool(true));
                case 'f' -> literal("false", new Bool(false));
                case 'n' -> literal("null", new Null());
                default -> number();
            };
        }

        Value literal(String word, Value v) {
            if (!s.startsWith(word, pos)) throw err("expected " + word);
            pos += word.length();
            return v;
        }

        Obj object() {
            expect('{');
            Map<String, Value> fields = new LinkedHashMap<>();
            skipWs();
            if (peek() == '}') { pos++; return new Obj(fields); }
            while (true) {
                skipWs();
                String key = string();
                skipWs();
                expect(':');
                skipWs();
                fields.put(key, value());
                skipWs();
                char c = next();
                if (c == '}') break;
                if (c != ',') throw err("expected , or } in object");
            }
            return new Obj(fields);
        }

        Arr array() {
            expect('[');
            List<Value> items = new ArrayList<>();
            skipWs();
            if (peek() == ']') { pos++; return new Arr(items); }
            while (true) {
                skipWs();
                items.add(value());
                skipWs();
                char c = next();
                if (c == ']') break;
                if (c != ',') throw err("expected , or ] in array");
            }
            return new Arr(items);
        }

        String string() {
            expect('"');
            StringBuilder sb = new StringBuilder();
            while (true) {
                char c = next();
                if (c == '"') break;
                if (c != '\\') { sb.append(c); continue; }
                char esc = next();
                switch (esc) {
                    case '"' -> sb.append('"');
                    case '\\' -> sb.append('\\');
                    case '/' -> sb.append('/');
                    case 'b' -> sb.append('\b');
                    case 'f' -> sb.append('\f');
                    case 'n' -> sb.append('\n');
                    case 'r' -> sb.append('\r');
                    case 't' -> sb.append('\t');
                    case 'u' -> {
                        sb.append((char) Integer.parseInt(s.substring(pos, pos + 4), 16));
                        pos += 4;
                    }
                    default -> throw err("bad escape \\" + esc);
                }
            }
            return sb.toString();
        }

        Num number() {
            int start = pos;
            if (peek() == '-' || peek() == '+') pos++;
            while (pos < s.length()) {
                char c = s.charAt(pos);
                if ((c >= '0' && c <= '9') || c == '.' || c == 'e' || c == 'E' || c == '-' || c == '+') pos++;
                else break;
            }
            if (start == pos) throw err("expected a number");
            return new Num(Double.parseDouble(s.substring(start, pos)));
        }

        char peek() { return pos < s.length() ? s.charAt(pos) : '\0'; }
        char next() {
            if (pos >= s.length()) throw err("unexpected end of input");
            return s.charAt(pos++);
        }
        void expect(char c) {
            if (next() != c) throw err("expected " + c);
        }
        IllegalArgumentException err(String msg) {
            return new IllegalArgumentException(msg + " at offset " + pos);
        }
    }

    // -- writing -------------------------------------------------------------

    /** Minimal writer for the state frames the service emits. */
    public static final class Writer {
        private final StringBuilder sb = new StringBuilder(256);
        private boolean needComma;

        public Writer open() { sb.append('{'); needComma = false; return this; }
        public Writer close() { sb.append('}'); needComma = true; return this; }

        public Writer put(String key, double value) {
            comma(); key(key);
            if (value == Math.rint(value) && !Double.isInfinite(value)) {
                sb.append((long) value);
            } else {
                sb.append(Math.round(value * 1000.0) / 1000.0);
            }
            return this;
        }

        public Writer put(String key, String value) {
            comma(); key(key);
            sb.append('"');
            for (int i = 0; i < value.length(); i++) {
                char c = value.charAt(i);
                switch (c) {
                    case '"' -> sb.append("\\\"");
                    case '\\' -> sb.append("\\\\");
                    case '\n' -> sb.append("\\n");
                    case '\r' -> sb.append("\\r");
                    case '\t' -> sb.append("\\t");
                    default -> {
                        if (c < 0x20) sb.append(String.format("\\u%04x", (int) c));
                        else sb.append(c);
                    }
                }
            }
            sb.append('"');
            return this;
        }

        public Writer putArray(String key, float[] values) {
            comma(); key(key);
            sb.append('[');
            for (int i = 0; i < values.length; i++) {
                if (i > 0) sb.append(',');
                sb.append(Math.round(values[i] * 10f) / 10f);
            }
            sb.append(']');
            return this;
        }

        private void comma() { if (needComma) sb.append(','); needComma = true; }
        private void key(String k) { sb.append('"').append(k).append("\":"); }

        @Override public String toString() { return sb.toString(); }
    }
}
