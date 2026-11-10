import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;

public class BenchmarkServer {
  public static void main(String[] args) throws Exception {
    try (var input = new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8))) {
      for (String path; (path = input.readLine()) != null; ) {
        IndexingBench.main(new String[]{path});
        System.out.flush();
      }
    }
  }
}
