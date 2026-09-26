package ai.flybrain;

import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.ServerSocket;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.util.Base64;
import java.util.List;
import java.util.concurrent.CopyOnWriteArrayList;

/**
 * A WebSocket server, text frames only, no dependencies.
 *
 * <p>The brain produces a small JSON frame every 16 ms and clients send back
 * short commands. That is the entire protocol, so the handshake and two frame
 * opcodes are all that is needed here. Pulling in a servlet container to move
 * 200 bytes at 60 Hz would be the larger piece of engineering.
 *
 * <p>Client to server, one JSON object per frame:
 * <pre>
 *   {"threat":{"bearing":0.4,"size":12.5}}   something is out there
 *   {"threat":null}                          nothing is
 *   {"turn":1.8}                             the body rotated this fast (rad/s)
 * </pre>
 * Server to client: a {@link BrainService.MotorState} per tick.
 */
public final class WsServer implements AutoCloseable {

    private static final String GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

    private final ServerSocket server;
    private final BrainService service;
    private final List<Client> clients = new CopyOnWriteArrayList<>();
    private volatile boolean running = true;

    public WsServer(int port, BrainService service) throws IOException {
        this.server = new ServerSocket(port);
        this.service = service;
    }

    public int port() { return server.getLocalPort(); }
    public int clientCount() { return clients.size(); }

    /** Accept connections until closed. Run on its own thread. */
    public void acceptLoop() {
        while (running) {
            try {
                Socket socket = server.accept();
                socket.setTcpNoDelay(true);   // 200-byte frames at 60 Hz; Nagle would batch them
                Client client = new Client(socket);
                clients.add(client);
                Thread.ofVirtual().name("fly-ws-client").start(client::readLoop);
            } catch (IOException e) {
                if (running) System.err.println("accept failed: " + e.getMessage());
            }
        }
    }

    /** Push the current motor state to everyone. Called from the broadcast tick. */
    public void broadcast() {
        BrainService.MotorState state = service.snapshot();
        if (state == null) return;
        byte[] payload = state.toJson().getBytes(StandardCharsets.UTF_8);
        for (Client c : clients) c.sendText(payload);
    }

    @Override public void close() throws IOException {
        running = false;
        for (Client c : clients) c.closeQuietly();
        server.close();
    }

    // ------------------------------------------------------------------------

    private final class Client {
        private final Socket socket;
        private InputStream in;
        private OutputStream out;
        private boolean open;

        Client(Socket socket) { this.socket = socket; }

        void readLoop() {
            try {
                in = socket.getInputStream();
                out = socket.getOutputStream();
                if (!handshake()) { closeQuietly(); return; }
                open = true;

                while (running && open) {
                    String msg = readTextFrame();
                    if (msg == null) break;
                    handle(msg);
                }
            } catch (IOException e) {
                // A client going away mid-frame is ordinary, not an error.
            } finally {
                closeQuietly();
                clients.remove(this);
            }
        }

        private boolean handshake() throws IOException {
            StringBuilder head = new StringBuilder();
            int b, consecutiveNewlines = 0;
            while ((b = in.read()) != -1) {
                head.append((char) b);
                if (b == '\n') { if (++consecutiveNewlines == 2) break; }
                else if (b != '\r') consecutiveNewlines = 0;
            }

            String key = null;
            for (String line : head.toString().split("\r\n")) {
                if (line.toLowerCase().startsWith("sec-websocket-key:")) {
                    key = line.substring(line.indexOf(':') + 1).trim();
                }
            }
            if (key == null) return false;

            String accept;
            try {
                MessageDigest sha1 = MessageDigest.getInstance("SHA-1");
                accept = Base64.getEncoder().encodeToString(
                        sha1.digest((key + GUID).getBytes(StandardCharsets.UTF_8)));
            } catch (Exception e) {
                return false;
            }

            out.write(("HTTP/1.1 101 Switching Protocols\r\n"
                    + "Upgrade: websocket\r\n"
                    + "Connection: Upgrade\r\n"
                    + "Sec-WebSocket-Accept: " + accept + "\r\n\r\n")
                    .getBytes(StandardCharsets.UTF_8));
            out.flush();
            return true;
        }

        private String readTextFrame() throws IOException {
            int b0 = in.read();
            if (b0 == -1) return null;
            int opcode = b0 & 0x0F;
            if (opcode == 0x8) return null;            // close

            int b1 = in.read();
            if (b1 == -1) return null;
            boolean masked = (b1 & 0x80) != 0;
            long len = b1 & 0x7F;

            if (len == 126) {
                len = ((long) in.read() << 8) | in.read();
            } else if (len == 127) {
                len = 0;
                for (int i = 0; i < 8; i++) len = (len << 8) | in.read();
            }
            if (len > 1 << 20) throw new IOException("frame too large: " + len);

            byte[] mask = new byte[4];
            if (masked) readFully(mask);

            byte[] payload = new byte[(int) len];
            readFully(payload);
            if (masked) {
                for (int i = 0; i < payload.length; i++) payload[i] ^= mask[i & 3];
            }

            if (opcode == 0x9) {                        // ping -> pong
                sendFrame(0xA, payload);
                return "";
            }
            if (opcode != 0x1) return "";               // ignore binary and continuation
            return new String(payload, StandardCharsets.UTF_8);
        }

        private void readFully(byte[] dst) throws IOException {
            int off = 0;
            while (off < dst.length) {
                int n = in.read(dst, off, dst.length - off);
                if (n < 0) throw new IOException("stream closed mid-frame");
                off += n;
            }
        }

        void sendText(byte[] payload) {
            try {
                if (open) sendFrame(0x1, payload);
            } catch (IOException e) {
                closeQuietly();
            }
        }

        private synchronized void sendFrame(int opcode, byte[] payload) throws IOException {
            out.write(0x80 | opcode);
            int len = payload.length;
            if (len < 126) {
                out.write(len);
            } else if (len < 65536) {
                out.write(126);
                out.write(len >> 8);
                out.write(len & 0xFF);
            } else {
                out.write(127);
                for (int i = 7; i >= 0; i--) out.write((int) ((long) len >> (8 * i)) & 0xFF);
            }
            out.write(payload);
            out.flush();
        }

        private void handle(String msg) {
            if (msg.isEmpty()) return;
            try {
                Json.Obj o = Json.parse(msg).asObj();

                Json.Value threat = o.get("threat");
                if (threat != null) {
                    if (threat instanceof Json.Null) {
                        service.clearThreat();
                    } else {
                        Json.Obj t = threat.asObj();
                        service.setThreat((float) t.num("bearing"), (float) t.num("size"));
                    }
                }

                Json.Value turn = o.get("turn");
                if (turn != null) service.reportTurn((float) turn.asDouble());

            } catch (RuntimeException e) {
                // A malformed frame costs one tick of input, not the connection.
            }
        }

        void closeQuietly() {
            open = false;
            try { socket.close(); } catch (IOException ignored) { }
        }
    }
}
