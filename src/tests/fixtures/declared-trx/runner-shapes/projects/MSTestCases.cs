using Microsoft.VisualStudio.TestTools.UnitTesting;

[assembly: DoNotParallelize]

namespace DeclaredMSTestCapture;

[TestClass]
public class CaptureCases
{
#if CAPTURE_PASS
    [TestMethod]
    public void Passing() => Assert.AreEqual(4, 2 + 2);
#elif CAPTURE_FAIL
    [TestMethod]
    public void Failing() => Assert.Fail("Intentional captured test failure.");
#endif
}
